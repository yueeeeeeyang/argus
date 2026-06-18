//! 文件职责：维护独立设置窗口的打开、置前和关闭状态。
//! 创建日期：2026-06-12
//! 修改日期：2026-06-18
//! 作者：Argus 开发团队
//! 主要功能：将设置页从标签页迁移到无标题栏独立窗口，同时复用主应用配置、日志搜索和升级偏好持久化逻辑。

use std::borrow::Borrow;
use std::ops::Range;

use gpui::{
    AppContext, Bounds, ClipboardItem, Context, Keystroke, WindowBounds, WindowOptions, px, size,
};

use crate::app::{ArgusApp, InputTextSelectionDrag, SettingsTextInputState};
use crate::platform::open_with_registration::{
    register_open_with, registration_status, unregister_open_with,
};
use crate::text_selection::{
    TextSelectionGranularity, character_count, insert_text_at_character_index,
    remove_character_range, slice_character_range, word_range_at,
};
use crate::ui::settings_window::SettingsWindow;

/// 设置窗口默认宽度，保证单页设置内容不过度拉伸。
const SETTINGS_WINDOW_WIDTH: f32 = 760.0;
/// 设置窗口默认高度。
const SETTINGS_WINDOW_HEIGHT: f32 = 560.0;
/// 设置窗口最小宽度。
const SETTINGS_WINDOW_MIN_WIDTH: f32 = 560.0;
/// 设置窗口最小高度。
const SETTINGS_WINDOW_MIN_HEIGHT: f32 = 420.0;

impl ArgusApp {
    /// 打开设置独立窗口；若窗口已存在，则直接激活并显示到最前。
    ///
    /// 参数说明：
    /// - `cx`：主应用上下文，用于创建或激活独立窗口。
    pub fn open_settings_window(&mut self, cx: &mut Context<Self>) {
        self.refresh_open_with_registration_status(cx);

        if self.is_settings_window_open {
            if let Some(window_handle) = self.settings_window_handle.clone()
                && window_handle
                    .update(cx, |_, window, _| window.activate_window())
                    .is_ok()
            {
                self.placeholder_notice = "设置窗口已显示到最前".to_string();
                return;
            }

            self.is_settings_window_open = false;
            self.settings_window_handle = None;
        }

        let app_entity = cx.entity();
        let initial_theme = self.theme.clone();
        let initial_snapshot = SettingsWindow::snapshot_from_app(self);
        let bounds = Bounds::centered(
            None,
            size(px(SETTINGS_WINDOW_WIDTH), px(SETTINGS_WINDOW_HEIGHT)),
            cx,
        );
        let window_options = WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(
                px(SETTINGS_WINDOW_MIN_WIDTH),
                px(SETTINGS_WINDOW_MIN_HEIGHT),
            )),
            ..Default::default()
        };

        self.is_settings_window_open = true;
        self.is_theme_dropdown_open = false;
        self.placeholder_notice = "已打开设置窗口".to_string();

        match cx.open_window(window_options, move |_, cx| {
            cx.new(|cx| SettingsWindow::new(app_entity, initial_theme, initial_snapshot, cx))
        }) {
            Ok(window_handle) => {
                self.settings_window_handle = Some(window_handle);
            }
            Err(error) => {
                self.is_settings_window_open = false;
                self.settings_window_handle = None;
                self.placeholder_notice = format!("打开设置窗口失败：{error}");
            }
        }
    }

    /// 清理设置窗口打开状态；窗口关闭按钮和系统关闭事件都走该入口。
    pub fn close_settings_window(&mut self) {
        self.is_settings_window_open = false;
        self.settings_window_handle = None;
        self.is_theme_dropdown_open = false;
        self.placeholder_notice = "已关闭设置窗口".to_string();
    }

    /// 刷新系统“用 Argus 打开”右键菜单注册状态。
    ///
    /// 说明：状态查询应保持轻量，打开设置窗口和注册/卸载完成后都会调用；忙碌时跳过，
    /// 避免执行中状态被同步查询覆盖。
    pub fn refresh_open_with_registration_status(&mut self, _cx: &mut Context<Self>) {
        if self.is_open_with_registration_busy {
            return;
        }

        self.open_with_registration_status = registration_status();
    }

    /// 注册系统右键菜单；执行期间禁用注册/卸载按钮，避免重复写入系统状态。
    pub fn register_open_with_menu(&mut self, cx: &mut Context<Self>) {
        if self.is_open_with_registration_busy {
            self.open_with_registration_message = Some("系统右键菜单操作正在执行".to_string());
            return;
        }

        self.is_open_with_registration_busy = true;
        self.open_with_registration_message = Some("正在注册系统右键菜单...".to_string());
        self.placeholder_notice = "正在注册系统右键菜单".to_string();

        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { register_open_with() })
                .await;

            view.update(cx, |app, cx| {
                app.is_open_with_registration_busy = false;
                match result {
                    Ok(()) => {
                        app.open_with_registration_status = registration_status();
                        app.open_with_registration_message = Some("系统右键菜单已注册".to_string());
                        app.placeholder_notice = "系统右键菜单已注册".to_string();
                    }
                    Err(error) => {
                        app.open_with_registration_status = registration_status();
                        app.open_with_registration_message = Some(error.to_string());
                        app.placeholder_notice = format!("系统右键菜单注册失败：{error}");
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 卸载系统右键菜单；完成后重新查询系统状态并更新设置窗口提示。
    pub fn unregister_open_with_menu(&mut self, cx: &mut Context<Self>) {
        if self.is_open_with_registration_busy {
            self.open_with_registration_message = Some("系统右键菜单操作正在执行".to_string());
            return;
        }

        self.is_open_with_registration_busy = true;
        self.open_with_registration_message = Some("正在卸载系统右键菜单...".to_string());
        self.placeholder_notice = "正在卸载系统右键菜单".to_string();

        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { unregister_open_with() })
                .await;

            view.update(cx, |app, cx| {
                app.is_open_with_registration_busy = false;
                match result {
                    Ok(()) => {
                        app.open_with_registration_status = registration_status();
                        app.open_with_registration_message = Some("系统右键菜单已卸载".to_string());
                        app.placeholder_notice = "系统右键菜单已卸载".to_string();
                    }
                    Err(error) => {
                        app.open_with_registration_status = registration_status();
                        app.open_with_registration_message = Some(error.to_string());
                        app.placeholder_notice = format!("系统右键菜单卸载失败：{error}");
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 聚焦设置窗口快搜关键字输入框，并关闭设置页的其它浮层。
    pub fn focus_settings_quick_keywords_input(&mut self) {
        self.is_theme_dropdown_open = false;
        self.settings_jstack_thread_name_filter_input.is_focused = false;
        self.settings_jstack_thread_name_filter_input.marked_range = None;
        self.settings_jstack_stack_segment_filter_input.is_focused = false;
        self.settings_jstack_stack_segment_filter_input.marked_range = None;
        self.settings_upgrade_server_input.is_focused = false;
        self.settings_upgrade_server_input.marked_range = None;
        self.settings_upgrade_public_key_input.is_focused = false;
        self.settings_upgrade_public_key_input.marked_range = None;
        self.settings_quick_keywords_input.is_focused = true;
        self.settings_quick_keywords_input.marked_range = None;
    }

    /// 返回设置窗口快搜关键字输入框当前选区范围。
    ///
    /// 返回值：存在非空选区时返回字符范围；无选区或空选区返回 `None`。
    pub fn settings_quick_keywords_selection_range(&self) -> Option<Range<usize>> {
        normalized_input_selection_range(&self.settings_quick_keywords_input)
    }

    /// 清空设置窗口快搜关键字输入框，并立即持久化配置。
    pub fn clear_settings_quick_keywords_input(&mut self) {
        self.settings_quick_keywords_input.value.clear();
        self.settings_quick_keywords_input.cursor = 0;
        self.settings_quick_keywords_input.selection_anchor = None;
        self.settings_quick_keywords_input.marked_range = None;
        self.settings_quick_keywords_input.selection_drag = None;
        self.commit_settings_quick_keywords_input();
    }

    /// 直接更新快搜关键字配置；测试和未来批量设置入口可复用。
    pub fn update_settings_quick_keywords(&mut self, value: String) {
        self.settings_quick_keywords_input = SettingsTextInputState::from_value(value);
        self.commit_settings_quick_keywords_input();
    }

    /// 处理设置窗口快搜关键字输入框键盘事件。
    ///
    /// 参数说明：
    /// - `keystroke`：GPUI 归一化按键事件。
    /// - `cx`：主应用上下文，用于访问系统剪贴板。
    pub fn handle_settings_quick_keywords_key(
        &mut self,
        keystroke: &Keystroke,
        cx: &mut Context<Self>,
    ) {
        let key = keystroke.key.as_str();
        let modifiers = keystroke.modifiers;

        if modifiers.platform && key.eq_ignore_ascii_case("a") {
            self.select_all_settings_quick_keywords_input();
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("c") {
            self.copy_settings_quick_keywords_selection(cx);
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("x") {
            self.cut_settings_quick_keywords_selection(cx);
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("v") {
            self.paste_settings_quick_keywords_clipboard(cx);
            return;
        }

        match key {
            "backspace" => self.delete_settings_quick_keywords_backward(),
            "delete" => self.delete_settings_quick_keywords_forward(),
            "left" => self.move_settings_quick_keywords_cursor_left(modifiers.shift),
            "right" => self.move_settings_quick_keywords_cursor_right(modifiers.shift),
            "home" => self.move_settings_quick_keywords_cursor_to(0, modifiers.shift),
            "end" => {
                let text_length = character_count(&self.settings_quick_keywords_input.value);
                self.move_settings_quick_keywords_cursor_to(text_length, modifiers.shift);
            }
            "escape" => self.settings_quick_keywords_input.is_focused = false,
            _ if key.chars().count() == 1 && !modifiers.control && !modifiers.platform => {
                self.insert_settings_quick_keywords_text(key);
            }
            _ => {}
        }
    }

    /// 开始设置窗口快搜关键字输入框鼠标选择。
    pub fn begin_settings_quick_keywords_pointer_selection(
        &mut self,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        self.focus_settings_quick_keywords_input();
        let range = settings_input_range_for_granularity(
            &self.settings_quick_keywords_input,
            character_index,
            granularity,
        );
        self.settings_quick_keywords_input.cursor = range.end;
        self.settings_quick_keywords_input.selection_anchor = Some(range.start);
        self.settings_quick_keywords_input.marked_range = None;
        self.settings_quick_keywords_input.selection_drag = Some(InputTextSelectionDrag {
            anchor_range: range,
            granularity,
        });
    }

    /// 更新设置窗口快搜关键字输入框鼠标拖拽选择。
    pub fn update_settings_quick_keywords_pointer_selection(&mut self, character_index: usize) {
        let Some(drag) = self.settings_quick_keywords_input.selection_drag.clone() else {
            return;
        };
        let focus_range = settings_input_range_for_granularity(
            &self.settings_quick_keywords_input,
            character_index,
            drag.granularity,
        );
        let start = drag.anchor_range.start.min(focus_range.start);
        let end = drag.anchor_range.end.max(focus_range.end);
        self.settings_quick_keywords_input.selection_anchor = Some(start);
        self.settings_quick_keywords_input.marked_range = None;
        self.settings_quick_keywords_input.cursor = end;
    }

    /// 结束设置窗口快搜关键字输入框鼠标选择。
    pub fn finish_settings_quick_keywords_pointer_selection(&mut self) {
        self.settings_quick_keywords_input.selection_drag = None;
    }

    /// 将设置输入框内容写回配置并保存。
    fn commit_settings_quick_keywords_input(&mut self) {
        self.config.log_search.quick_keywords = self.settings_quick_keywords_input.value.clone();
        self.placeholder_notice = "快搜关键字已保存".to_string();
        self.persist_config_or_report();
    }

    /// 向设置快搜输入框插入文本。
    fn insert_settings_quick_keywords_text(&mut self, text: &str) {
        self.delete_settings_quick_keywords_selection();
        let input = &mut self.settings_quick_keywords_input;
        input.value = insert_text_at_character_index(&input.value, input.cursor, text);
        input.cursor += character_count(text);
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_quick_keywords_input();
    }

    /// 删除设置快搜输入框当前选区。
    fn delete_settings_quick_keywords_selection(&mut self) -> bool {
        let Some(range) = self.settings_quick_keywords_selection_range() else {
            return false;
        };
        let input = &mut self.settings_quick_keywords_input;
        input.value = remove_character_range(&input.value, range.clone());
        input.cursor = range.start;
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_quick_keywords_input();
        true
    }

    /// 从光标前删除一个字符。
    fn delete_settings_quick_keywords_backward(&mut self) {
        if self.delete_settings_quick_keywords_selection()
            || self.settings_quick_keywords_input.cursor == 0
        {
            return;
        }
        let cursor = self.settings_quick_keywords_input.cursor;
        let input = &mut self.settings_quick_keywords_input;
        input.value = remove_character_range(&input.value, cursor - 1..cursor);
        input.cursor -= 1;
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_quick_keywords_input();
    }

    /// 从光标后删除一个字符。
    fn delete_settings_quick_keywords_forward(&mut self) {
        if self.delete_settings_quick_keywords_selection() {
            return;
        }
        let cursor = self.settings_quick_keywords_input.cursor;
        let text_length = character_count(&self.settings_quick_keywords_input.value);
        if cursor >= text_length {
            return;
        }
        let input = &mut self.settings_quick_keywords_input;
        input.value = remove_character_range(&input.value, cursor..cursor + 1);
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_quick_keywords_input();
    }

    /// 左移设置快搜输入框光标。
    fn move_settings_quick_keywords_cursor_left(&mut self, extend_selection: bool) {
        let cursor = self.settings_quick_keywords_input.cursor.saturating_sub(1);
        self.move_settings_quick_keywords_cursor_to(cursor, extend_selection);
    }

    /// 右移设置快搜输入框光标。
    fn move_settings_quick_keywords_cursor_right(&mut self, extend_selection: bool) {
        let text_length = character_count(&self.settings_quick_keywords_input.value);
        let cursor = (self.settings_quick_keywords_input.cursor + 1).min(text_length);
        self.move_settings_quick_keywords_cursor_to(cursor, extend_selection);
    }

    /// 移动设置快搜输入框光标，并按需扩展选区。
    fn move_settings_quick_keywords_cursor_to(&mut self, cursor: usize, extend_selection: bool) {
        let text_length = character_count(&self.settings_quick_keywords_input.value);
        let cursor = cursor.min(text_length);
        let input = &mut self.settings_quick_keywords_input;
        if extend_selection {
            input.selection_anchor.get_or_insert(input.cursor);
        } else {
            input.selection_anchor = None;
        }
        input.cursor = cursor;
        input.marked_range = None;
        input.selection_drag = None;
    }

    /// 全选设置快搜输入框文本。
    fn select_all_settings_quick_keywords_input(&mut self) {
        self.settings_quick_keywords_input.selection_anchor = Some(0);
        self.settings_quick_keywords_input.cursor =
            character_count(&self.settings_quick_keywords_input.value);
        self.settings_quick_keywords_input.marked_range = None;
        self.settings_quick_keywords_input.selection_drag = None;
    }

    /// 复制设置快搜输入框选中文本。
    fn copy_settings_quick_keywords_selection(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.selected_settings_quick_keywords_text() else {
            return;
        };
        let app_context: &gpui::App = (&*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// 剪切设置快搜输入框选中文本。
    fn cut_settings_quick_keywords_selection(&mut self, cx: &mut Context<Self>) {
        self.copy_settings_quick_keywords_selection(cx);
        self.delete_settings_quick_keywords_selection();
    }

    /// 粘贴剪贴板文本到设置快搜输入框。
    fn paste_settings_quick_keywords_clipboard(&mut self, cx: &mut Context<Self>) {
        let app_context: &gpui::App = (&*cx).borrow();
        let Some(item) = app_context.read_from_clipboard() else {
            return;
        };
        if let Some(text) = item.text() {
            self.insert_settings_quick_keywords_text(&text.replace(['\n', '\r'], " "));
        }
    }

    /// 返回设置快搜输入框选中文本。
    fn selected_settings_quick_keywords_text(&self) -> Option<String> {
        let range = self.settings_quick_keywords_selection_range()?;
        Some(slice_character_range(
            &self.settings_quick_keywords_input.value,
            range,
        ))
    }

    /// 聚焦设置窗口 Jstack 线程名过滤输入框。
    pub fn focus_settings_jstack_thread_name_filter_input(&mut self) {
        self.is_theme_dropdown_open = false;
        self.settings_quick_keywords_input.is_focused = false;
        self.settings_quick_keywords_input.marked_range = None;
        self.settings_jstack_stack_segment_filter_input.is_focused = false;
        self.settings_jstack_stack_segment_filter_input.marked_range = None;
        self.settings_upgrade_server_input.is_focused = false;
        self.settings_upgrade_server_input.marked_range = None;
        self.settings_upgrade_public_key_input.is_focused = false;
        self.settings_upgrade_public_key_input.marked_range = None;
        self.settings_jstack_thread_name_filter_input.is_focused = true;
        self.settings_jstack_thread_name_filter_input.marked_range = None;
    }

    /// 返回设置窗口 Jstack 线程名过滤输入框当前选区范围。
    pub fn settings_jstack_thread_name_filter_selection_range(&self) -> Option<Range<usize>> {
        normalized_input_selection_range(&self.settings_jstack_thread_name_filter_input)
    }

    /// 清空 Jstack 线程名过滤输入框，并立即持久化配置。
    pub fn clear_settings_jstack_thread_name_filter_input(&mut self) {
        self.settings_jstack_thread_name_filter_input.value.clear();
        self.settings_jstack_thread_name_filter_input.cursor = 0;
        self.settings_jstack_thread_name_filter_input
            .selection_anchor = None;
        self.settings_jstack_thread_name_filter_input.marked_range = None;
        self.settings_jstack_thread_name_filter_input.selection_drag = None;
        self.commit_settings_jstack_thread_name_filter_input();
    }

    /// 直接更新 Jstack 线程名过滤配置，便于测试和未来批量导入复用。
    pub fn update_settings_jstack_thread_name_filter(&mut self, value: String) {
        self.settings_jstack_thread_name_filter_input = SettingsTextInputState::from_value(value);
        self.commit_settings_jstack_thread_name_filter_input();
    }

    /// 处理设置窗口 Jstack 线程名过滤输入框键盘事件。
    pub fn handle_settings_jstack_thread_name_filter_key(
        &mut self,
        keystroke: &Keystroke,
        cx: &mut Context<Self>,
    ) {
        let key = keystroke.key.as_str();
        let modifiers = keystroke.modifiers;

        if modifiers.platform && key.eq_ignore_ascii_case("a") {
            self.select_all_settings_jstack_thread_name_filter_input();
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("c") {
            self.copy_settings_jstack_thread_name_filter_selection(cx);
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("x") {
            self.cut_settings_jstack_thread_name_filter_selection(cx);
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("v") {
            self.paste_settings_jstack_thread_name_filter_clipboard(cx);
            return;
        }

        match key {
            "backspace" => self.delete_settings_jstack_thread_name_filter_backward(),
            "delete" => self.delete_settings_jstack_thread_name_filter_forward(),
            "left" => self.move_settings_jstack_thread_name_filter_cursor_left(modifiers.shift),
            "right" => self.move_settings_jstack_thread_name_filter_cursor_right(modifiers.shift),
            "home" => self.move_settings_jstack_thread_name_filter_cursor_to(0, modifiers.shift),
            "end" => {
                let text_length =
                    character_count(&self.settings_jstack_thread_name_filter_input.value);
                self.move_settings_jstack_thread_name_filter_cursor_to(
                    text_length,
                    modifiers.shift,
                );
            }
            "escape" => self.settings_jstack_thread_name_filter_input.is_focused = false,
            _ if key.chars().count() == 1 && !modifiers.control && !modifiers.platform => {
                self.insert_settings_jstack_thread_name_filter_text(key);
            }
            _ => {}
        }
    }

    /// 开始设置窗口 Jstack 线程名过滤输入框鼠标选择。
    pub fn begin_settings_jstack_thread_name_filter_pointer_selection(
        &mut self,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        self.focus_settings_jstack_thread_name_filter_input();
        let range = settings_input_range_for_granularity(
            &self.settings_jstack_thread_name_filter_input,
            character_index,
            granularity,
        );
        self.settings_jstack_thread_name_filter_input.cursor = range.end;
        self.settings_jstack_thread_name_filter_input
            .selection_anchor = Some(range.start);
        self.settings_jstack_thread_name_filter_input.marked_range = None;
        self.settings_jstack_thread_name_filter_input.selection_drag =
            Some(InputTextSelectionDrag {
                anchor_range: range,
                granularity,
            });
    }

    /// 更新设置窗口 Jstack 线程名过滤输入框鼠标拖拽选择。
    pub fn update_settings_jstack_thread_name_filter_pointer_selection(
        &mut self,
        character_index: usize,
    ) {
        let Some(drag) = self
            .settings_jstack_thread_name_filter_input
            .selection_drag
            .clone()
        else {
            return;
        };
        let focus_range = settings_input_range_for_granularity(
            &self.settings_jstack_thread_name_filter_input,
            character_index,
            drag.granularity,
        );
        let start = drag.anchor_range.start.min(focus_range.start);
        let end = drag.anchor_range.end.max(focus_range.end);
        self.settings_jstack_thread_name_filter_input
            .selection_anchor = Some(start);
        self.settings_jstack_thread_name_filter_input.marked_range = None;
        self.settings_jstack_thread_name_filter_input.cursor = end;
    }

    /// 结束设置窗口 Jstack 线程名过滤输入框鼠标选择。
    pub fn finish_settings_jstack_thread_name_filter_pointer_selection(&mut self) {
        self.settings_jstack_thread_name_filter_input.selection_drag = None;
    }

    /// 将 Jstack 线程名过滤输入框内容写回配置并保存。
    fn commit_settings_jstack_thread_name_filter_input(&mut self) {
        self.config.log_display.jstack_thread_name_filters = self
            .settings_jstack_thread_name_filter_input
            .value
            .trim()
            .to_string();
        self.rebuild_all_jstack_visible_row_caches();
        self.placeholder_notice = "Jstack 线程名过滤已保存".to_string();
        self.persist_config_or_report();
    }

    /// 向 Jstack 线程名过滤输入框插入文本。
    fn insert_settings_jstack_thread_name_filter_text(&mut self, text: &str) {
        self.delete_settings_jstack_thread_name_filter_selection();
        let input = &mut self.settings_jstack_thread_name_filter_input;
        input.value = insert_text_at_character_index(&input.value, input.cursor, text);
        input.cursor += character_count(text);
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_jstack_thread_name_filter_input();
    }

    /// 删除 Jstack 线程名过滤输入框当前选区。
    fn delete_settings_jstack_thread_name_filter_selection(&mut self) -> bool {
        let Some(range) = self.settings_jstack_thread_name_filter_selection_range() else {
            return false;
        };
        let input = &mut self.settings_jstack_thread_name_filter_input;
        input.value = remove_character_range(&input.value, range.clone());
        input.cursor = range.start;
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_jstack_thread_name_filter_input();
        true
    }

    /// 从 Jstack 线程名过滤输入框光标前删除一个字符。
    fn delete_settings_jstack_thread_name_filter_backward(&mut self) {
        if self.delete_settings_jstack_thread_name_filter_selection()
            || self.settings_jstack_thread_name_filter_input.cursor == 0
        {
            return;
        }
        let cursor = self.settings_jstack_thread_name_filter_input.cursor;
        let input = &mut self.settings_jstack_thread_name_filter_input;
        input.value = remove_character_range(&input.value, cursor - 1..cursor);
        input.cursor -= 1;
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_jstack_thread_name_filter_input();
    }

    /// 从 Jstack 线程名过滤输入框光标后删除一个字符。
    fn delete_settings_jstack_thread_name_filter_forward(&mut self) {
        if self.delete_settings_jstack_thread_name_filter_selection() {
            return;
        }
        let cursor = self.settings_jstack_thread_name_filter_input.cursor;
        let text_length = character_count(&self.settings_jstack_thread_name_filter_input.value);
        if cursor >= text_length {
            return;
        }
        let input = &mut self.settings_jstack_thread_name_filter_input;
        input.value = remove_character_range(&input.value, cursor..cursor + 1);
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_jstack_thread_name_filter_input();
    }

    /// 左移 Jstack 线程名过滤输入框光标。
    fn move_settings_jstack_thread_name_filter_cursor_left(&mut self, extend_selection: bool) {
        let cursor = self
            .settings_jstack_thread_name_filter_input
            .cursor
            .saturating_sub(1);
        self.move_settings_jstack_thread_name_filter_cursor_to(cursor, extend_selection);
    }

    /// 右移 Jstack 线程名过滤输入框光标。
    fn move_settings_jstack_thread_name_filter_cursor_right(&mut self, extend_selection: bool) {
        let text_length = character_count(&self.settings_jstack_thread_name_filter_input.value);
        let cursor = (self.settings_jstack_thread_name_filter_input.cursor + 1).min(text_length);
        self.move_settings_jstack_thread_name_filter_cursor_to(cursor, extend_selection);
    }

    /// 移动 Jstack 线程名过滤输入框光标，并按需扩展选区。
    fn move_settings_jstack_thread_name_filter_cursor_to(
        &mut self,
        cursor: usize,
        extend_selection: bool,
    ) {
        let text_length = character_count(&self.settings_jstack_thread_name_filter_input.value);
        let cursor = cursor.min(text_length);
        let input = &mut self.settings_jstack_thread_name_filter_input;
        if extend_selection {
            input.selection_anchor.get_or_insert(input.cursor);
        } else {
            input.selection_anchor = None;
        }
        input.cursor = cursor;
        input.marked_range = None;
        input.selection_drag = None;
    }

    /// 全选 Jstack 线程名过滤输入框文本。
    fn select_all_settings_jstack_thread_name_filter_input(&mut self) {
        self.settings_jstack_thread_name_filter_input
            .selection_anchor = Some(0);
        self.settings_jstack_thread_name_filter_input.cursor =
            character_count(&self.settings_jstack_thread_name_filter_input.value);
        self.settings_jstack_thread_name_filter_input.marked_range = None;
        self.settings_jstack_thread_name_filter_input.selection_drag = None;
    }

    /// 复制 Jstack 线程名过滤输入框选中文本。
    fn copy_settings_jstack_thread_name_filter_selection(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.selected_settings_jstack_thread_name_filter_text() else {
            return;
        };
        let app_context: &gpui::App = (&*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// 剪切 Jstack 线程名过滤输入框选中文本。
    fn cut_settings_jstack_thread_name_filter_selection(&mut self, cx: &mut Context<Self>) {
        self.copy_settings_jstack_thread_name_filter_selection(cx);
        self.delete_settings_jstack_thread_name_filter_selection();
    }

    /// 粘贴剪贴板文本到 Jstack 线程名过滤输入框。
    fn paste_settings_jstack_thread_name_filter_clipboard(&mut self, cx: &mut Context<Self>) {
        let app_context: &gpui::App = (&*cx).borrow();
        let Some(item) = app_context.read_from_clipboard() else {
            return;
        };
        if let Some(text) = item.text() {
            self.insert_settings_jstack_thread_name_filter_text(&text.replace(['\n', '\r'], ","));
        }
    }

    /// 返回 Jstack 线程名过滤输入框选中文本。
    fn selected_settings_jstack_thread_name_filter_text(&self) -> Option<String> {
        let range = self.settings_jstack_thread_name_filter_selection_range()?;
        Some(slice_character_range(
            &self.settings_jstack_thread_name_filter_input.value,
            range,
        ))
    }

    /// 聚焦设置窗口 Jstack 完整线程段过滤输入框。
    pub fn focus_settings_jstack_stack_segment_filter_input(&mut self) {
        self.is_theme_dropdown_open = false;
        self.settings_quick_keywords_input.is_focused = false;
        self.settings_quick_keywords_input.marked_range = None;
        self.settings_jstack_thread_name_filter_input.is_focused = false;
        self.settings_jstack_thread_name_filter_input.marked_range = None;
        self.settings_upgrade_server_input.is_focused = false;
        self.settings_upgrade_server_input.marked_range = None;
        self.settings_upgrade_public_key_input.is_focused = false;
        self.settings_upgrade_public_key_input.marked_range = None;
        self.settings_jstack_stack_segment_filter_input.is_focused = true;
        self.settings_jstack_stack_segment_filter_input.marked_range = None;
    }

    /// 返回设置窗口 Jstack 完整线程段过滤输入框当前选区范围。
    pub fn settings_jstack_stack_segment_filter_selection_range(&self) -> Option<Range<usize>> {
        normalized_input_selection_range(&self.settings_jstack_stack_segment_filter_input)
    }

    /// 清空 Jstack 完整线程段过滤输入框，并立即持久化配置。
    pub fn clear_settings_jstack_stack_segment_filter_input(&mut self) {
        self.settings_jstack_stack_segment_filter_input
            .value
            .clear();
        self.settings_jstack_stack_segment_filter_input.cursor = 0;
        self.settings_jstack_stack_segment_filter_input
            .selection_anchor = None;
        self.settings_jstack_stack_segment_filter_input.marked_range = None;
        self.settings_jstack_stack_segment_filter_input
            .selection_drag = None;
        self.commit_settings_jstack_stack_segment_filter_input();
    }

    /// 直接更新 Jstack 完整线程段过滤配置，便于测试和未来批量导入复用。
    pub fn update_settings_jstack_stack_segment_filter(&mut self, value: String) {
        self.settings_jstack_stack_segment_filter_input = SettingsTextInputState::from_value(value);
        self.commit_settings_jstack_stack_segment_filter_input();
    }

    /// 处理设置窗口 Jstack 完整线程段过滤输入框键盘事件。
    pub fn handle_settings_jstack_stack_segment_filter_key(
        &mut self,
        keystroke: &Keystroke,
        cx: &mut Context<Self>,
    ) {
        let key = keystroke.key.as_str();
        let modifiers = keystroke.modifiers;

        if modifiers.platform && key.eq_ignore_ascii_case("a") {
            self.select_all_settings_jstack_stack_segment_filter_input();
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("c") {
            self.copy_settings_jstack_stack_segment_filter_selection(cx);
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("x") {
            self.cut_settings_jstack_stack_segment_filter_selection(cx);
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("v") {
            self.paste_settings_jstack_stack_segment_filter_clipboard(cx);
            return;
        }

        match key {
            "backspace" => self.delete_settings_jstack_stack_segment_filter_backward(),
            "delete" => self.delete_settings_jstack_stack_segment_filter_forward(),
            "enter" => self.insert_settings_jstack_stack_segment_filter_text("\n"),
            "left" => self.move_settings_jstack_stack_segment_filter_cursor_left(modifiers.shift),
            "right" => self.move_settings_jstack_stack_segment_filter_cursor_right(modifiers.shift),
            "up" => self
                .move_settings_jstack_stack_segment_filter_cursor_vertically(-1, modifiers.shift),
            "down" => {
                self.move_settings_jstack_stack_segment_filter_cursor_vertically(1, modifiers.shift)
            }
            "home" => {
                let cursor = current_line_range(
                    &self.settings_jstack_stack_segment_filter_input.value,
                    self.settings_jstack_stack_segment_filter_input.cursor,
                )
                .start;
                self.move_settings_jstack_stack_segment_filter_cursor_to(cursor, modifiers.shift);
            }
            "end" => {
                let cursor = current_line_range(
                    &self.settings_jstack_stack_segment_filter_input.value,
                    self.settings_jstack_stack_segment_filter_input.cursor,
                )
                .end;
                self.move_settings_jstack_stack_segment_filter_cursor_to(cursor, modifiers.shift);
            }
            "escape" => {
                self.settings_jstack_stack_segment_filter_input.is_focused = false;
                self.settings_jstack_stack_segment_filter_input.marked_range = None;
                self.settings_jstack_stack_segment_filter_input
                    .selection_drag = None;
            }
            _ if key.chars().count() == 1
                && !modifiers.control
                && !modifiers.platform
                && !key.chars().any(char::is_control) =>
            {
                self.insert_settings_jstack_stack_segment_filter_text(key);
            }
            _ => {}
        }
    }

    /// 开始设置窗口 Jstack 完整线程段过滤输入框鼠标选择。
    pub fn begin_settings_jstack_stack_segment_filter_pointer_selection(
        &mut self,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        self.focus_settings_jstack_stack_segment_filter_input();
        let range = settings_textarea_range_for_granularity(
            &self.settings_jstack_stack_segment_filter_input,
            character_index,
            granularity,
        );
        self.settings_jstack_stack_segment_filter_input.cursor = range.end;
        self.settings_jstack_stack_segment_filter_input
            .selection_anchor = Some(range.start);
        self.settings_jstack_stack_segment_filter_input.marked_range = None;
        self.settings_jstack_stack_segment_filter_input
            .selection_drag = Some(InputTextSelectionDrag {
            anchor_range: range,
            granularity,
        });
    }

    /// 更新设置窗口 Jstack 完整线程段过滤输入框鼠标拖拽选择。
    pub fn update_settings_jstack_stack_segment_filter_pointer_selection(
        &mut self,
        character_index: usize,
    ) {
        let Some(drag) = self
            .settings_jstack_stack_segment_filter_input
            .selection_drag
            .clone()
        else {
            return;
        };
        let focus_range = settings_textarea_range_for_granularity(
            &self.settings_jstack_stack_segment_filter_input,
            character_index,
            drag.granularity,
        );
        let start = drag.anchor_range.start.min(focus_range.start);
        let end = drag.anchor_range.end.max(focus_range.end);
        self.settings_jstack_stack_segment_filter_input
            .selection_anchor = Some(start);
        self.settings_jstack_stack_segment_filter_input.marked_range = None;
        self.settings_jstack_stack_segment_filter_input.cursor = end;
    }

    /// 结束设置窗口 Jstack 完整线程段过滤输入框鼠标选择。
    pub fn finish_settings_jstack_stack_segment_filter_pointer_selection(&mut self) {
        self.settings_jstack_stack_segment_filter_input
            .selection_drag = None;
    }

    /// 将 Jstack 完整线程段过滤输入框内容写回配置并保存。
    fn commit_settings_jstack_stack_segment_filter_input(&mut self) {
        self.config.log_display.jstack_stack_segment_filters =
            normalized_textarea_value(&self.settings_jstack_stack_segment_filter_input.value);
        self.rebuild_all_jstack_visible_row_caches();
        self.placeholder_notice = "Jstack 线程段过滤已保存".to_string();
        self.persist_config_or_report();
    }

    /// 向 Jstack 完整线程段过滤输入框插入文本。
    fn insert_settings_jstack_stack_segment_filter_text(&mut self, text: &str) {
        self.delete_settings_jstack_stack_segment_filter_selection();
        let text = normalized_textarea_value(text);
        let input = &mut self.settings_jstack_stack_segment_filter_input;
        input.value = insert_text_at_character_index(&input.value, input.cursor, &text);
        input.cursor += character_count(&text);
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_jstack_stack_segment_filter_input();
    }

    /// 删除 Jstack 完整线程段过滤输入框当前选区。
    fn delete_settings_jstack_stack_segment_filter_selection(&mut self) -> bool {
        let Some(range) = self.settings_jstack_stack_segment_filter_selection_range() else {
            return false;
        };
        let input = &mut self.settings_jstack_stack_segment_filter_input;
        input.value = remove_character_range(&input.value, range.clone());
        input.cursor = range.start;
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_jstack_stack_segment_filter_input();
        true
    }

    /// 从 Jstack 完整线程段过滤输入框光标前删除一个字符。
    fn delete_settings_jstack_stack_segment_filter_backward(&mut self) {
        if self.delete_settings_jstack_stack_segment_filter_selection()
            || self.settings_jstack_stack_segment_filter_input.cursor == 0
        {
            return;
        }
        let cursor = self.settings_jstack_stack_segment_filter_input.cursor;
        let input = &mut self.settings_jstack_stack_segment_filter_input;
        input.value = remove_character_range(&input.value, cursor - 1..cursor);
        input.cursor -= 1;
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_jstack_stack_segment_filter_input();
    }

    /// 从 Jstack 完整线程段过滤输入框光标后删除一个字符。
    fn delete_settings_jstack_stack_segment_filter_forward(&mut self) {
        if self.delete_settings_jstack_stack_segment_filter_selection() {
            return;
        }
        let cursor = self.settings_jstack_stack_segment_filter_input.cursor;
        let text_length = character_count(&self.settings_jstack_stack_segment_filter_input.value);
        if cursor >= text_length {
            return;
        }
        let input = &mut self.settings_jstack_stack_segment_filter_input;
        input.value = remove_character_range(&input.value, cursor..cursor + 1);
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_jstack_stack_segment_filter_input();
    }

    /// 左移 Jstack 完整线程段过滤输入框光标。
    fn move_settings_jstack_stack_segment_filter_cursor_left(&mut self, extend_selection: bool) {
        let cursor = self
            .settings_jstack_stack_segment_filter_input
            .cursor
            .saturating_sub(1);
        self.move_settings_jstack_stack_segment_filter_cursor_to(cursor, extend_selection);
    }

    /// 右移 Jstack 完整线程段过滤输入框光标。
    fn move_settings_jstack_stack_segment_filter_cursor_right(&mut self, extend_selection: bool) {
        let text_length = character_count(&self.settings_jstack_stack_segment_filter_input.value);
        let cursor = (self.settings_jstack_stack_segment_filter_input.cursor + 1).min(text_length);
        self.move_settings_jstack_stack_segment_filter_cursor_to(cursor, extend_selection);
    }

    /// 上下移动 Jstack 完整线程段过滤输入框光标，尽量保持当前列位置。
    fn move_settings_jstack_stack_segment_filter_cursor_vertically(
        &mut self,
        direction: isize,
        extend_selection: bool,
    ) {
        let value = &self.settings_jstack_stack_segment_filter_input.value;
        let cursor = self.settings_jstack_stack_segment_filter_input.cursor;
        let next_cursor = vertical_cursor_position(value, cursor, direction);
        self.move_settings_jstack_stack_segment_filter_cursor_to(next_cursor, extend_selection);
    }

    /// 移动 Jstack 完整线程段过滤输入框光标，并按需扩展选区。
    fn move_settings_jstack_stack_segment_filter_cursor_to(
        &mut self,
        cursor: usize,
        extend_selection: bool,
    ) {
        let text_length = character_count(&self.settings_jstack_stack_segment_filter_input.value);
        let cursor = cursor.min(text_length);
        let input = &mut self.settings_jstack_stack_segment_filter_input;
        if extend_selection {
            input.selection_anchor.get_or_insert(input.cursor);
        } else {
            input.selection_anchor = None;
        }
        input.cursor = cursor;
        input.marked_range = None;
        input.selection_drag = None;
    }

    /// 全选 Jstack 完整线程段过滤输入框文本。
    fn select_all_settings_jstack_stack_segment_filter_input(&mut self) {
        self.settings_jstack_stack_segment_filter_input
            .selection_anchor = Some(0);
        self.settings_jstack_stack_segment_filter_input.cursor =
            character_count(&self.settings_jstack_stack_segment_filter_input.value);
        self.settings_jstack_stack_segment_filter_input.marked_range = None;
        self.settings_jstack_stack_segment_filter_input
            .selection_drag = None;
    }

    /// 复制 Jstack 完整线程段过滤输入框选中文本。
    fn copy_settings_jstack_stack_segment_filter_selection(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.selected_settings_jstack_stack_segment_filter_text() else {
            return;
        };
        let app_context: &gpui::App = (&*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// 剪切 Jstack 完整线程段过滤输入框选中文本。
    fn cut_settings_jstack_stack_segment_filter_selection(&mut self, cx: &mut Context<Self>) {
        self.copy_settings_jstack_stack_segment_filter_selection(cx);
        self.delete_settings_jstack_stack_segment_filter_selection();
    }

    /// 粘贴剪贴板文本到 Jstack 完整线程段过滤输入框；保留真实换行以匹配完整线程段。
    fn paste_settings_jstack_stack_segment_filter_clipboard(&mut self, cx: &mut Context<Self>) {
        let app_context: &gpui::App = (&*cx).borrow();
        let Some(item) = app_context.read_from_clipboard() else {
            return;
        };
        if let Some(text) = item.text() {
            self.insert_settings_jstack_stack_segment_filter_text(&text);
        }
    }

    /// 返回 Jstack 完整线程段过滤输入框选中文本。
    fn selected_settings_jstack_stack_segment_filter_text(&self) -> Option<String> {
        let range = self.settings_jstack_stack_segment_filter_selection_range()?;
        Some(slice_character_range(
            &self.settings_jstack_stack_segment_filter_input.value,
            range,
        ))
    }

    /// 聚焦设置窗口升级服务器输入框，并关闭设置页的其它浮层。
    pub fn focus_settings_upgrade_server_input(&mut self) {
        self.is_theme_dropdown_open = false;
        self.settings_quick_keywords_input.is_focused = false;
        self.settings_quick_keywords_input.marked_range = None;
        self.settings_jstack_thread_name_filter_input.is_focused = false;
        self.settings_jstack_thread_name_filter_input.marked_range = None;
        self.settings_jstack_stack_segment_filter_input.is_focused = false;
        self.settings_jstack_stack_segment_filter_input.marked_range = None;
        self.settings_upgrade_public_key_input.is_focused = false;
        self.settings_upgrade_public_key_input.marked_range = None;
        self.settings_upgrade_server_input.is_focused = true;
        self.settings_upgrade_server_input.marked_range = None;
    }

    /// 返回设置窗口升级服务器输入框当前选区范围。
    pub fn settings_upgrade_server_selection_range(&self) -> Option<Range<usize>> {
        normalized_input_selection_range(&self.settings_upgrade_server_input)
    }

    /// 清空升级服务器输入框，并立即持久化配置。
    pub fn clear_settings_upgrade_server_input(&mut self) {
        self.settings_upgrade_server_input.value.clear();
        self.settings_upgrade_server_input.cursor = 0;
        self.settings_upgrade_server_input.selection_anchor = None;
        self.settings_upgrade_server_input.marked_range = None;
        self.settings_upgrade_server_input.selection_drag = None;
        self.commit_settings_upgrade_server_input();
    }

    /// 直接更新升级服务器地址；测试和未来导入配置入口可复用。
    pub fn update_settings_upgrade_server_url(&mut self, value: String) {
        self.settings_upgrade_server_input = SettingsTextInputState::from_value(value);
        self.commit_settings_upgrade_server_input();
    }

    /// 处理设置窗口升级服务器输入框键盘事件。
    pub fn handle_settings_upgrade_server_key(
        &mut self,
        keystroke: &Keystroke,
        cx: &mut Context<Self>,
    ) {
        let key = keystroke.key.as_str();
        let modifiers = keystroke.modifiers;

        if modifiers.platform && key.eq_ignore_ascii_case("a") {
            self.select_all_settings_upgrade_server_input();
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("c") {
            self.copy_settings_upgrade_server_selection(cx);
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("x") {
            self.cut_settings_upgrade_server_selection(cx);
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("v") {
            self.paste_settings_upgrade_server_clipboard(cx);
            return;
        }

        match key {
            "backspace" => self.delete_settings_upgrade_server_backward(),
            "delete" => self.delete_settings_upgrade_server_forward(),
            "left" => self.move_settings_upgrade_server_cursor_left(modifiers.shift),
            "right" => self.move_settings_upgrade_server_cursor_right(modifiers.shift),
            "home" => self.move_settings_upgrade_server_cursor_to(0, modifiers.shift),
            "end" => {
                let text_length = character_count(&self.settings_upgrade_server_input.value);
                self.move_settings_upgrade_server_cursor_to(text_length, modifiers.shift);
            }
            "escape" => self.settings_upgrade_server_input.is_focused = false,
            _ if key.chars().count() == 1 && !modifiers.control && !modifiers.platform => {
                self.insert_settings_upgrade_server_text(key);
            }
            _ => {}
        }
    }

    /// 开始设置窗口升级服务器输入框鼠标选择。
    pub fn begin_settings_upgrade_server_pointer_selection(
        &mut self,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        self.focus_settings_upgrade_server_input();
        let range = settings_input_range_for_granularity(
            &self.settings_upgrade_server_input,
            character_index,
            granularity,
        );
        self.settings_upgrade_server_input.cursor = range.end;
        self.settings_upgrade_server_input.selection_anchor = Some(range.start);
        self.settings_upgrade_server_input.marked_range = None;
        self.settings_upgrade_server_input.selection_drag = Some(InputTextSelectionDrag {
            anchor_range: range,
            granularity,
        });
    }

    /// 更新设置窗口升级服务器输入框鼠标拖拽选择。
    pub fn update_settings_upgrade_server_pointer_selection(&mut self, character_index: usize) {
        let Some(drag) = self.settings_upgrade_server_input.selection_drag.clone() else {
            return;
        };
        let focus_range = settings_input_range_for_granularity(
            &self.settings_upgrade_server_input,
            character_index,
            drag.granularity,
        );
        let start = drag.anchor_range.start.min(focus_range.start);
        let end = drag.anchor_range.end.max(focus_range.end);
        self.settings_upgrade_server_input.selection_anchor = Some(start);
        self.settings_upgrade_server_input.marked_range = None;
        self.settings_upgrade_server_input.cursor = end;
    }

    /// 结束设置窗口升级服务器输入框鼠标选择。
    pub fn finish_settings_upgrade_server_pointer_selection(&mut self) {
        self.settings_upgrade_server_input.selection_drag = None;
    }

    /// 将升级服务器输入框内容写回配置并保存。
    fn commit_settings_upgrade_server_input(&mut self) {
        self.config.upgrade.server_url =
            self.settings_upgrade_server_input.value.trim().to_string();
        self.placeholder_notice = "升级服务器已保存".to_string();
        self.persist_config_or_report();
    }

    /// 向升级服务器输入框插入文本。
    fn insert_settings_upgrade_server_text(&mut self, text: &str) {
        self.delete_settings_upgrade_server_selection();
        let input = &mut self.settings_upgrade_server_input;
        input.value = insert_text_at_character_index(&input.value, input.cursor, text);
        input.cursor += character_count(text);
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_upgrade_server_input();
    }

    /// 删除升级服务器输入框当前选区。
    fn delete_settings_upgrade_server_selection(&mut self) -> bool {
        let Some(range) = self.settings_upgrade_server_selection_range() else {
            return false;
        };
        let input = &mut self.settings_upgrade_server_input;
        input.value = remove_character_range(&input.value, range.clone());
        input.cursor = range.start;
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_upgrade_server_input();
        true
    }

    /// 从升级服务器输入框光标前删除一个字符。
    fn delete_settings_upgrade_server_backward(&mut self) {
        if self.delete_settings_upgrade_server_selection()
            || self.settings_upgrade_server_input.cursor == 0
        {
            return;
        }
        let cursor = self.settings_upgrade_server_input.cursor;
        let input = &mut self.settings_upgrade_server_input;
        input.value = remove_character_range(&input.value, cursor - 1..cursor);
        input.cursor -= 1;
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_upgrade_server_input();
    }

    /// 从升级服务器输入框光标后删除一个字符。
    fn delete_settings_upgrade_server_forward(&mut self) {
        if self.delete_settings_upgrade_server_selection() {
            return;
        }
        let cursor = self.settings_upgrade_server_input.cursor;
        let text_length = character_count(&self.settings_upgrade_server_input.value);
        if cursor >= text_length {
            return;
        }
        let input = &mut self.settings_upgrade_server_input;
        input.value = remove_character_range(&input.value, cursor..cursor + 1);
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_upgrade_server_input();
    }

    /// 左移升级服务器输入框光标。
    fn move_settings_upgrade_server_cursor_left(&mut self, extend_selection: bool) {
        let cursor = self.settings_upgrade_server_input.cursor.saturating_sub(1);
        self.move_settings_upgrade_server_cursor_to(cursor, extend_selection);
    }

    /// 右移升级服务器输入框光标。
    fn move_settings_upgrade_server_cursor_right(&mut self, extend_selection: bool) {
        let text_length = character_count(&self.settings_upgrade_server_input.value);
        let cursor = (self.settings_upgrade_server_input.cursor + 1).min(text_length);
        self.move_settings_upgrade_server_cursor_to(cursor, extend_selection);
    }

    /// 移动升级服务器输入框光标，并按需扩展选区。
    fn move_settings_upgrade_server_cursor_to(&mut self, cursor: usize, extend_selection: bool) {
        let text_length = character_count(&self.settings_upgrade_server_input.value);
        let cursor = cursor.min(text_length);
        let input = &mut self.settings_upgrade_server_input;
        if extend_selection {
            input.selection_anchor.get_or_insert(input.cursor);
        } else {
            input.selection_anchor = None;
        }
        input.cursor = cursor;
        input.marked_range = None;
        input.selection_drag = None;
    }

    /// 全选升级服务器输入框文本。
    fn select_all_settings_upgrade_server_input(&mut self) {
        self.settings_upgrade_server_input.selection_anchor = Some(0);
        self.settings_upgrade_server_input.cursor =
            character_count(&self.settings_upgrade_server_input.value);
        self.settings_upgrade_server_input.marked_range = None;
        self.settings_upgrade_server_input.selection_drag = None;
    }

    /// 复制升级服务器输入框选中文本。
    fn copy_settings_upgrade_server_selection(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.selected_settings_upgrade_server_text() else {
            return;
        };
        let app_context: &gpui::App = (&*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// 剪切升级服务器输入框选中文本。
    fn cut_settings_upgrade_server_selection(&mut self, cx: &mut Context<Self>) {
        self.copy_settings_upgrade_server_selection(cx);
        self.delete_settings_upgrade_server_selection();
    }

    /// 粘贴剪贴板文本到升级服务器输入框。
    fn paste_settings_upgrade_server_clipboard(&mut self, cx: &mut Context<Self>) {
        let app_context: &gpui::App = (&*cx).borrow();
        let Some(item) = app_context.read_from_clipboard() else {
            return;
        };
        if let Some(text) = item.text() {
            self.insert_settings_upgrade_server_text(&text.replace(['\n', '\r'], " "));
        }
    }

    /// 返回升级服务器输入框选中文本。
    fn selected_settings_upgrade_server_text(&self) -> Option<String> {
        let range = self.settings_upgrade_server_selection_range()?;
        Some(slice_character_range(
            &self.settings_upgrade_server_input.value,
            range,
        ))
    }

    /// 聚焦设置窗口升级验签公钥输入框，并关闭设置页的其它浮层。
    pub fn focus_settings_upgrade_public_key_input(&mut self) {
        self.is_theme_dropdown_open = false;
        self.settings_quick_keywords_input.is_focused = false;
        self.settings_quick_keywords_input.marked_range = None;
        self.settings_jstack_thread_name_filter_input.is_focused = false;
        self.settings_jstack_thread_name_filter_input.marked_range = None;
        self.settings_jstack_stack_segment_filter_input.is_focused = false;
        self.settings_jstack_stack_segment_filter_input.marked_range = None;
        self.settings_upgrade_server_input.is_focused = false;
        self.settings_upgrade_server_input.marked_range = None;
        self.settings_upgrade_public_key_input.is_focused = true;
        self.settings_upgrade_public_key_input.marked_range = None;
    }

    /// 返回设置窗口升级验签公钥输入框当前选区范围。
    pub fn settings_upgrade_public_key_selection_range(&self) -> Option<Range<usize>> {
        normalized_input_selection_range(&self.settings_upgrade_public_key_input)
    }

    /// 清空升级验签公钥输入框，并立即持久化配置。
    pub fn clear_settings_upgrade_public_key_input(&mut self) {
        self.settings_upgrade_public_key_input.value.clear();
        self.settings_upgrade_public_key_input.cursor = 0;
        self.settings_upgrade_public_key_input.selection_anchor = None;
        self.settings_upgrade_public_key_input.marked_range = None;
        self.settings_upgrade_public_key_input.selection_drag = None;
        self.commit_settings_upgrade_public_key_input();
    }

    /// 直接更新升级验签公钥；测试和未来导入配置入口可复用。
    pub fn update_settings_upgrade_public_key(&mut self, value: String) {
        self.settings_upgrade_public_key_input = SettingsTextInputState::from_value(value);
        self.commit_settings_upgrade_public_key_input();
    }

    /// 处理设置窗口升级验签公钥输入框键盘事件。
    ///
    /// 参数说明：
    /// - `keystroke`：GPUI 归一化按键事件。
    /// - `cx`：主应用上下文，用于访问系统剪贴板。
    pub fn handle_settings_upgrade_public_key_key(
        &mut self,
        keystroke: &Keystroke,
        cx: &mut Context<Self>,
    ) {
        let key = keystroke.key.as_str();
        let modifiers = keystroke.modifiers;

        if modifiers.platform && key.eq_ignore_ascii_case("a") {
            self.select_all_settings_upgrade_public_key_input();
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("c") {
            self.copy_settings_upgrade_public_key_selection(cx);
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("x") {
            self.cut_settings_upgrade_public_key_selection(cx);
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("v") {
            self.paste_settings_upgrade_public_key_clipboard(cx);
            return;
        }

        match key {
            "backspace" => self.delete_settings_upgrade_public_key_backward(),
            "delete" => self.delete_settings_upgrade_public_key_forward(),
            "left" => self.move_settings_upgrade_public_key_cursor_left(modifiers.shift),
            "right" => self.move_settings_upgrade_public_key_cursor_right(modifiers.shift),
            "home" => self.move_settings_upgrade_public_key_cursor_to(0, modifiers.shift),
            "end" => {
                let text_length = character_count(&self.settings_upgrade_public_key_input.value);
                self.move_settings_upgrade_public_key_cursor_to(text_length, modifiers.shift);
            }
            "escape" => self.settings_upgrade_public_key_input.is_focused = false,
            _ if key.chars().count() == 1 && !modifiers.control && !modifiers.platform => {
                self.insert_settings_upgrade_public_key_text(key);
            }
            _ => {}
        }
    }

    /// 开始设置窗口升级验签公钥输入框鼠标选择。
    pub fn begin_settings_upgrade_public_key_pointer_selection(
        &mut self,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        self.focus_settings_upgrade_public_key_input();
        let range = settings_input_range_for_granularity(
            &self.settings_upgrade_public_key_input,
            character_index,
            granularity,
        );
        self.settings_upgrade_public_key_input.cursor = range.end;
        self.settings_upgrade_public_key_input.selection_anchor = Some(range.start);
        self.settings_upgrade_public_key_input.marked_range = None;
        self.settings_upgrade_public_key_input.selection_drag = Some(InputTextSelectionDrag {
            anchor_range: range,
            granularity,
        });
    }

    /// 更新设置窗口升级验签公钥输入框鼠标拖拽选择。
    pub fn update_settings_upgrade_public_key_pointer_selection(&mut self, character_index: usize) {
        let Some(drag) = self
            .settings_upgrade_public_key_input
            .selection_drag
            .clone()
        else {
            return;
        };
        let focus_range = settings_input_range_for_granularity(
            &self.settings_upgrade_public_key_input,
            character_index,
            drag.granularity,
        );
        let start = drag.anchor_range.start.min(focus_range.start);
        let end = drag.anchor_range.end.max(focus_range.end);
        self.settings_upgrade_public_key_input.selection_anchor = Some(start);
        self.settings_upgrade_public_key_input.marked_range = None;
        self.settings_upgrade_public_key_input.cursor = end;
    }

    /// 结束设置窗口升级验签公钥输入框鼠标选择。
    pub fn finish_settings_upgrade_public_key_pointer_selection(&mut self) {
        self.settings_upgrade_public_key_input.selection_drag = None;
    }

    /// 将升级验签公钥输入框内容写回配置并保存。
    fn commit_settings_upgrade_public_key_input(&mut self) {
        self.config.upgrade.public_key_base64 = self
            .settings_upgrade_public_key_input
            .value
            .trim()
            .to_string();
        self.placeholder_notice = "升级验签公钥已保存".to_string();
        self.persist_config_or_report();
    }

    /// 向升级验签公钥输入框插入文本。
    fn insert_settings_upgrade_public_key_text(&mut self, text: &str) {
        self.delete_settings_upgrade_public_key_selection();
        let input = &mut self.settings_upgrade_public_key_input;
        input.value = insert_text_at_character_index(&input.value, input.cursor, text);
        input.cursor += character_count(text);
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_upgrade_public_key_input();
    }

    /// 删除升级验签公钥输入框当前选区。
    fn delete_settings_upgrade_public_key_selection(&mut self) -> bool {
        let Some(range) = self.settings_upgrade_public_key_selection_range() else {
            return false;
        };
        let input = &mut self.settings_upgrade_public_key_input;
        input.value = remove_character_range(&input.value, range.clone());
        input.cursor = range.start;
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_upgrade_public_key_input();
        true
    }

    /// 从升级验签公钥输入框光标前删除一个字符。
    fn delete_settings_upgrade_public_key_backward(&mut self) {
        if self.delete_settings_upgrade_public_key_selection()
            || self.settings_upgrade_public_key_input.cursor == 0
        {
            return;
        }
        let cursor = self.settings_upgrade_public_key_input.cursor;
        let input = &mut self.settings_upgrade_public_key_input;
        input.value = remove_character_range(&input.value, cursor - 1..cursor);
        input.cursor -= 1;
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_upgrade_public_key_input();
    }

    /// 从升级验签公钥输入框光标后删除一个字符。
    fn delete_settings_upgrade_public_key_forward(&mut self) {
        if self.delete_settings_upgrade_public_key_selection() {
            return;
        }
        let cursor = self.settings_upgrade_public_key_input.cursor;
        let text_length = character_count(&self.settings_upgrade_public_key_input.value);
        if cursor >= text_length {
            return;
        }
        let input = &mut self.settings_upgrade_public_key_input;
        input.value = remove_character_range(&input.value, cursor..cursor + 1);
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_upgrade_public_key_input();
    }

    /// 左移升级验签公钥输入框光标。
    fn move_settings_upgrade_public_key_cursor_left(&mut self, extend_selection: bool) {
        let cursor = self
            .settings_upgrade_public_key_input
            .cursor
            .saturating_sub(1);
        self.move_settings_upgrade_public_key_cursor_to(cursor, extend_selection);
    }

    /// 右移升级验签公钥输入框光标。
    fn move_settings_upgrade_public_key_cursor_right(&mut self, extend_selection: bool) {
        let text_length = character_count(&self.settings_upgrade_public_key_input.value);
        let cursor = (self.settings_upgrade_public_key_input.cursor + 1).min(text_length);
        self.move_settings_upgrade_public_key_cursor_to(cursor, extend_selection);
    }

    /// 移动升级验签公钥输入框光标，并按需扩展选区。
    fn move_settings_upgrade_public_key_cursor_to(
        &mut self,
        cursor: usize,
        extend_selection: bool,
    ) {
        let text_length = character_count(&self.settings_upgrade_public_key_input.value);
        let cursor = cursor.min(text_length);
        let input = &mut self.settings_upgrade_public_key_input;
        if extend_selection {
            input.selection_anchor.get_or_insert(input.cursor);
        } else {
            input.selection_anchor = None;
        }
        input.cursor = cursor;
        input.marked_range = None;
        input.selection_drag = None;
    }

    /// 全选升级验签公钥输入框文本。
    fn select_all_settings_upgrade_public_key_input(&mut self) {
        self.settings_upgrade_public_key_input.selection_anchor = Some(0);
        self.settings_upgrade_public_key_input.cursor =
            character_count(&self.settings_upgrade_public_key_input.value);
        self.settings_upgrade_public_key_input.marked_range = None;
        self.settings_upgrade_public_key_input.selection_drag = None;
    }

    /// 复制升级验签公钥输入框选中文本。
    fn copy_settings_upgrade_public_key_selection(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.selected_settings_upgrade_public_key_text() else {
            return;
        };
        let app_context: &gpui::App = (&*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// 剪切升级验签公钥输入框选中文本。
    fn cut_settings_upgrade_public_key_selection(&mut self, cx: &mut Context<Self>) {
        self.copy_settings_upgrade_public_key_selection(cx);
        self.delete_settings_upgrade_public_key_selection();
    }

    /// 粘贴剪贴板文本到升级验签公钥输入框；去掉换行以兼容脚本输出和折行 Base64。
    fn paste_settings_upgrade_public_key_clipboard(&mut self, cx: &mut Context<Self>) {
        let app_context: &gpui::App = (&*cx).borrow();
        let Some(item) = app_context.read_from_clipboard() else {
            return;
        };
        if let Some(text) = item.text() {
            self.insert_settings_upgrade_public_key_text(&text.replace(['\n', '\r'], ""));
        }
    }

    /// 返回升级验签公钥输入框选中文本。
    fn selected_settings_upgrade_public_key_text(&self) -> Option<String> {
        let range = self.settings_upgrade_public_key_selection_range()?;
        Some(slice_character_range(
            &self.settings_upgrade_public_key_input.value,
            range,
        ))
    }
}

/// 返回输入状态中的规范化非空选区。
fn normalized_input_selection_range(input: &SettingsTextInputState) -> Option<Range<usize>> {
    let anchor = input.selection_anchor?;
    if anchor == input.cursor {
        return None;
    }
    Some(anchor.min(input.cursor)..anchor.max(input.cursor))
}

/// 根据鼠标选择粒度返回设置输入框目标字符范围。
fn settings_input_range_for_granularity(
    input: &SettingsTextInputState,
    character_index: usize,
    granularity: TextSelectionGranularity,
) -> Range<usize> {
    let text_length = character_count(&input.value);
    let cursor = character_index.min(text_length);
    match granularity {
        TextSelectionGranularity::Character => cursor..cursor,
        TextSelectionGranularity::Word => {
            word_range_at(&input.value, cursor).unwrap_or(cursor..cursor)
        }
        TextSelectionGranularity::Line => 0..text_length,
    }
}

/// 根据鼠标选择粒度返回多行设置输入框目标字符范围。
fn settings_textarea_range_for_granularity(
    input: &SettingsTextInputState,
    character_index: usize,
    granularity: TextSelectionGranularity,
) -> Range<usize> {
    let text_length = character_count(&input.value);
    let cursor = character_index.min(text_length);
    match granularity {
        TextSelectionGranularity::Character => cursor..cursor,
        TextSelectionGranularity::Word => {
            word_range_at(&input.value, cursor).unwrap_or(cursor..cursor)
        }
        TextSelectionGranularity::Line => current_line_range(&input.value, cursor),
    }
}

/// 归一化 textarea 文本，统一系统换行符但保留真实多行结构。
fn normalized_textarea_value(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

/// 返回光标所在行的字符范围，不包含行尾换行符。
fn current_line_range(value: &str, cursor: usize) -> Range<usize> {
    let chars = value.chars().collect::<Vec<_>>();
    let text_length = chars.len();
    let cursor = cursor.min(text_length);
    let mut start = cursor;
    while start > 0 && chars[start - 1] != '\n' {
        start -= 1;
    }
    let mut end = cursor;
    while end < text_length && chars[end] != '\n' {
        end += 1;
    }
    start..end
}

/// 上下移动多行文本光标，并尽量保持原始列位置。
fn vertical_cursor_position(value: &str, cursor: usize, direction: isize) -> usize {
    let chars = value.chars().collect::<Vec<_>>();
    let text_length = chars.len();
    let cursor = cursor.min(text_length);
    let current_line = current_line_range(value, cursor);
    let current_column = cursor.saturating_sub(current_line.start);

    if direction < 0 {
        if current_line.start == 0 {
            return cursor;
        }
        let previous_line_end = current_line.start - 1;
        let previous_line = current_line_range(value, previous_line_end);
        return previous_line.start
            + current_column.min(previous_line.end.saturating_sub(previous_line.start));
    }

    if current_line.end >= text_length {
        return cursor;
    }
    let next_line_start = current_line.end + 1;
    let next_line = current_line_range(value, next_line_start);
    next_line.start + current_column.min(next_line.end.saturating_sub(next_line.start))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 textarea 行范围只覆盖当前行，不会像单行输入框那样全选。
    #[test]
    fn textarea_line_granularity_selects_current_line() {
        let input = SettingsTextInputState::from_value("first\nsecond\nthird".to_string());

        let range =
            settings_textarea_range_for_granularity(&input, 8, TextSelectionGranularity::Line);

        assert_eq!(slice_character_range(&input.value, range), "second");
    }

    /// 验证 textarea 上下移动光标时尽量保持列位置，并在短行处夹到行尾。
    #[test]
    fn textarea_vertical_cursor_keeps_column_when_possible() {
        let value = "abcdef\nxy\n123456";

        assert_eq!(vertical_cursor_position(value, 4, 1), 9);
        assert_eq!(vertical_cursor_position(value, 9, 1), 12);
        assert_eq!(vertical_cursor_position(value, 12, -1), 9);
    }

    /// 验证 textarea 文本归一化会统一换行符但保留多行结构。
    #[test]
    fn textarea_normalization_preserves_real_newlines() {
        assert_eq!(normalized_textarea_value("a\r\nb\rc"), "a\nb\nc");
    }
}
