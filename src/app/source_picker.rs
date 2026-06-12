//! 文件职责：维护自定义日志来源选择器的应用状态与业务动作。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：替代系统文件选择器，提供跨平台目录浏览、多选和确认加载流程。

use std::cmp::Ordering;
use std::ops::Range;
use std::path::{Path, PathBuf};

use gpui::{
    AppContext, Bounds, Context, Keystroke, ScrollStrategy, UniformListScrollHandle, WindowBounds,
    WindowHandle, WindowOptions, px, size,
};

use crate::app::{ArgusApp, InputTextSelectionDrag};
use crate::loader::{BrowseEntry, BrowseLocation, BrowseResult, LogSourceLoader, PathBrowser};
use crate::text_selection::{
    TextSelectionGranularity, character_count, insert_text_at_character_index,
    remove_character_range, word_range_at,
};
use crate::ui::source_picker::SourcePickerWindow;
use crate::utils::path::display_path;

/// 来源选择器独立窗口默认宽度。
const SOURCE_PICKER_WINDOW_WIDTH: f32 = 900.0;
/// 来源选择器独立窗口默认高度。
const SOURCE_PICKER_WINDOW_HEIGHT: f32 = 620.0;
/// 来源选择器独立窗口最小宽度，保证主要控件不会拥挤。
const SOURCE_PICKER_WINDOW_MIN_WIDTH: f32 = 680.0;
/// 来源选择器独立窗口最小高度，保证列表和底部操作区可用。
const SOURCE_PICKER_WINDOW_MIN_HEIGHT: f32 = 460.0;

/// 来源选择器列表支持的排序字段。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourcePickerSortKey {
    /// 按条目名称排序。
    Name,
    /// 按文件系统修改日期排序。
    Modified,
}

/// 来源选择器列表排序方向。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourcePickerSortDirection {
    /// 升序排列。
    Ascending,
    /// 降序排列。
    Descending,
}

impl SourcePickerSortDirection {
    /// 返回当前方向的反向值，用于重复点击同一列表头。
    fn toggled(self) -> Self {
        match self {
            Self::Ascending => Self::Descending,
            Self::Descending => Self::Ascending,
        }
    }
}

/// 自定义来源选择器状态；所有字段只描述 UI 和待加载路径，不持有文件句柄。
#[derive(Clone, Debug)]
pub struct SourcePickerState {
    /// 选择器是否打开。
    pub is_open: bool,
    /// 当前选择器独立窗口句柄，用于重复点击加载入口时把已有窗口置前。
    pub window_handle: Option<WindowHandle<SourcePickerWindow>>,
    /// 当前正在浏览的目录。
    pub current_dir: PathBuf,
    /// 当前目录父级，根目录或盘符根为空。
    pub parent_dir: Option<PathBuf>,
    /// 左侧常用位置入口。
    pub locations: Vec<BrowseLocation>,
    /// 当前目录直接子项。
    pub entries: Vec<BrowseEntry>,
    /// 是否正在后台读取当前目录。
    pub is_loading: bool,
    /// 最近一次浏览失败或选择失败的提示。
    pub error_message: Option<String>,
    /// 已选中等待加载的本地路径。
    pub selected_paths: Vec<PathBuf>,
    /// 目录列表滚动句柄。
    pub entry_scroll: UniformListScrollHandle,
    /// 当前后台浏览 generation，用于丢弃旧结果。
    pub browse_generation: usize,
    /// 顶部手动路径输入框内容。
    pub path_input: String,
    /// 手动路径输入框光标位置，使用字符索引。
    pub path_input_cursor: usize,
    /// 手动路径输入框选区锚点。
    pub path_input_selection_anchor: Option<usize>,
    /// 手动路径输入框鼠标拖拽选择状态。
    pub path_input_selection_drag: Option<InputTextSelectionDrag>,
    /// 手动路径输入框是否聚焦。
    pub is_path_input_focused: bool,
    /// 当前目录列表排序字段。
    pub sort_key: SourcePickerSortKey,
    /// 当前目录列表排序方向。
    pub sort_direction: SourcePickerSortDirection,
}

impl Default for SourcePickerState {
    /// 构造选择器初始状态，默认定位用户主目录或当前工作目录。
    fn default() -> Self {
        let current_dir = PathBrowser::default_start_directory();
        let path_input = display_path(&current_dir);
        let path_input_cursor = character_count(&path_input);

        Self {
            is_open: false,
            window_handle: None,
            current_dir,
            parent_dir: None,
            locations: PathBrowser::default_locations(),
            entries: Vec::new(),
            is_loading: false,
            error_message: None,
            selected_paths: Vec::new(),
            entry_scroll: UniformListScrollHandle::new(),
            browse_generation: 0,
            path_input,
            path_input_cursor,
            path_input_selection_anchor: None,
            path_input_selection_drag: None,
            is_path_input_focused: false,
            sort_key: SourcePickerSortKey::Modified,
            sort_direction: SourcePickerSortDirection::Descending,
        }
    }
}

impl SourcePickerState {
    /// 返回输入框当前选区范围，空选区返回 `None`。
    pub fn path_input_selection_range(&self) -> Option<Range<usize>> {
        let anchor = self.path_input_selection_anchor?;
        let cursor = self.path_input_cursor;
        if anchor == cursor {
            return None;
        }

        Some(anchor.min(cursor)..anchor.max(cursor))
    }

    /// 判断路径是否已经被加入待加载列表。
    pub fn is_selected(&self, path: &Path) -> bool {
        self.selected_paths.iter().any(|selected| selected == path)
    }

    /// 把当前目录写回路径输入框，并把光标放到末尾。
    fn sync_path_input_to_current_dir(&mut self) {
        self.path_input = display_path(&self.current_dir);
        self.path_input_cursor = character_count(&self.path_input);
        self.path_input_selection_anchor = None;
        self.path_input_selection_drag = None;
    }

    /// 设置路径输入框为指定目录文案，通常用于加载中的即时反馈。
    fn set_path_input_from_path(&mut self, path: &Path) {
        self.path_input = display_path(path);
        self.path_input_cursor = character_count(&self.path_input);
        self.path_input_selection_anchor = None;
        self.path_input_selection_drag = None;
    }

    /// 将选择器恢复到每次新打开窗口时的默认浏览状态。
    ///
    /// 说明：默认定位下载目录并按修改日期倒序排列，符合日志包通常来自下载目录、
    /// 新文件更常被选择的使用路径。
    fn reset_for_open(&mut self) {
        let current_dir = PathBrowser::default_start_directory();
        self.current_dir = current_dir.clone();
        self.parent_dir = None;
        self.entries.clear();
        self.is_loading = false;
        self.error_message = None;
        self.selected_paths.clear();
        self.entry_scroll = UniformListScrollHandle::new();
        self.sort_key = SourcePickerSortKey::Modified;
        self.sort_direction = SourcePickerSortDirection::Descending;
        self.is_path_input_focused = false;
        self.set_path_input_from_path(&current_dir);
    }
}

impl ArgusApp {
    /// 打开自定义来源选择器独立窗口，并在后台刷新当前目录条目。
    pub fn open_source_picker(&mut self, cx: &mut Context<Self>) {
        if self.is_source_loading {
            self.placeholder_notice = "日志来源正在加载中，请稍候".to_string();
            return;
        }
        if self.source_picker.is_open {
            if let Some(window_handle) = self.source_picker.window_handle.clone()
                && window_handle
                    .update(cx, |_, window, _| window.activate_window())
                    .is_ok()
            {
                self.placeholder_notice = "日志来源选择器已显示到最前".to_string();
                return;
            }

            self.source_picker.is_open = false;
            self.source_picker.window_handle = None;
        }

        let app_entity = cx.entity();
        self.source_picker.is_open = true;
        self.source_picker.reset_for_open();
        self.source_picker.locations = PathBrowser::default_locations();
        self.placeholder_notice = "请选择日志文件、目录或压缩包".to_string();

        let directory = self.source_picker.current_dir.clone();
        self.navigate_source_picker(directory, cx);

        let initial_theme = self.theme.clone();
        let initial_source_picker = self.source_picker.clone();
        let bounds = Bounds::centered(
            None,
            size(
                px(SOURCE_PICKER_WINDOW_WIDTH),
                px(SOURCE_PICKER_WINDOW_HEIGHT),
            ),
            cx,
        );
        let window_options = WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(
                px(SOURCE_PICKER_WINDOW_MIN_WIDTH),
                px(SOURCE_PICKER_WINDOW_MIN_HEIGHT),
            )),
            ..Default::default()
        };

        match cx.open_window(window_options, move |_, cx| {
            cx.new(|cx| {
                SourcePickerWindow::new(app_entity, initial_theme, initial_source_picker, cx)
            })
        }) {
            Ok(window_handle) => {
                self.source_picker.window_handle = Some(window_handle);
            }
            Err(error) => {
                self.source_picker.is_open = false;
                self.source_picker.window_handle = None;
                self.source_picker.is_loading = false;
                self.source_picker.error_message = Some(error.to_string());
                self.placeholder_notice = format!("打开日志来源选择器失败：{error}");
            }
        }
    }

    /// 关闭自定义来源选择器，不影响已经加载的来源树。
    pub fn close_source_picker(&mut self) {
        self.source_picker.is_open = false;
        self.source_picker.window_handle = None;
        self.source_picker.is_loading = false;
        self.source_picker.error_message = None;
        self.source_picker.selected_paths.clear();
        self.source_picker.path_input_selection_drag = None;
        self.source_picker.is_path_input_focused = false;
        self.placeholder_notice = "已取消加载日志来源".to_string();
    }

    /// 后台浏览指定目录；成功后替换当前目录列表，失败时保留原目录。
    pub fn navigate_source_picker(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.source_picker.is_open = true;
        self.source_picker.is_loading = true;
        self.source_picker.error_message = None;
        self.source_picker.browse_generation = self.source_picker.browse_generation.wrapping_add(1);
        self.source_picker.entry_scroll = UniformListScrollHandle::new();
        self.source_picker.set_path_input_from_path(&path);
        let browse_generation = self.source_picker.browse_generation;

        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { PathBrowser::list_directory(path) })
                .await;

            view.update(cx, |app, cx| {
                app.apply_source_picker_browse_result(browse_generation, result);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 应用后台目录浏览结果，过期结果会被丢弃。
    fn apply_source_picker_browse_result(
        &mut self,
        browse_generation: usize,
        result: anyhow::Result<BrowseResult>,
    ) {
        if self.source_picker.browse_generation != browse_generation {
            return;
        }

        self.source_picker.is_loading = false;
        match result {
            Ok(result) => {
                let entry_count = result.entries.len();
                self.source_picker.current_dir = result.directory;
                self.source_picker.parent_dir = result.parent;
                self.source_picker.entries = result.entries;
                self.sort_source_picker_entries();
                self.source_picker.error_message = None;
                self.source_picker.sync_path_input_to_current_dir();
                self.source_picker
                    .entry_scroll
                    .scroll_to_item(0, ScrollStrategy::Top);
                self.placeholder_notice = format!("已读取目录，包含 {entry_count} 个条目");
            }
            Err(error) => {
                self.source_picker.sync_path_input_to_current_dir();
                self.source_picker.error_message = Some(error.to_string());
                self.placeholder_notice = format!("目录浏览失败：{error}");
            }
        }
    }

    /// 切换一个可选文件或压缩包路径的选中状态。
    pub fn toggle_source_picker_file(&mut self, path: PathBuf) {
        let Some(entry) = self
            .source_picker
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .cloned()
        else {
            self.source_picker.error_message = Some("未找到该文件条目".to_string());
            return;
        };

        if !entry.is_selectable {
            self.source_picker.error_message = entry.disabled_reason;
            return;
        }

        self.toggle_source_picker_path(path);
    }

    /// 切换目录行的选中状态；目录行双击才会进入浏览。
    pub fn toggle_source_picker_directory(&mut self, path: PathBuf) {
        self.toggle_source_picker_path(path);
    }

    /// 将当前目录加入待加载列表；保留给快捷入口和测试使用。
    pub fn select_source_picker_current_directory(&mut self) {
        let current_dir = self.source_picker.current_dir.clone();
        self.toggle_source_picker_path(current_dir);
    }

    /// 从待加载列表中移除指定路径。
    pub fn remove_source_picker_path(&mut self, path: &Path) {
        self.source_picker
            .selected_paths
            .retain(|selected| selected != path);
        self.source_picker.error_message = None;
    }

    /// 清空选择器里的所有待加载路径。
    pub fn clear_source_picker_selection(&mut self) {
        self.source_picker.selected_paths.clear();
        self.source_picker.error_message = None;
    }

    /// 确认选择器路径并复用现有来源加载流程，返回是否成功进入加载状态。
    pub fn confirm_source_picker_selection(&mut self, cx: &mut Context<Self>) -> bool {
        if self.is_source_loading {
            self.source_picker.error_message = Some("日志来源正在加载中，请稍候".to_string());
            return false;
        }

        let paths = self.source_picker.selected_paths.clone();
        if paths.is_empty() {
            self.source_picker.error_message = Some("请至少选择一个文件、压缩包或目录".to_string());
            return false;
        }

        self.source_picker.is_open = false;
        self.source_picker.window_handle = None;
        self.source_picker.error_message = None;
        self.is_source_loading = true;
        self.placeholder_notice = format!("正在加载 {} 个日志来源", paths.len());
        let loader_config = self.config.loader.clone();

        cx.spawn(async move |view, cx| {
            let report = cx
                .background_executor()
                .spawn(async move { LogSourceLoader::new(loader_config).load_paths(paths) })
                .await;

            view.update(cx, |app, cx| {
                app.apply_load_report(report);
                cx.notify();
            })
            .ok();
        })
        .detach();

        true
    }

    /// 返回路径是否在待加载选择列表中。
    pub fn is_source_picker_path_selected(&self, path: &Path) -> bool {
        self.source_picker.is_selected(path)
    }

    /// 切换来源选择器目录列表排序字段或方向。
    pub fn set_source_picker_sort(&mut self, sort_key: SourcePickerSortKey) {
        if self.source_picker.sort_key == sort_key {
            self.source_picker.sort_direction = self.source_picker.sort_direction.toggled();
        } else {
            self.source_picker.sort_key = sort_key;
            self.source_picker.sort_direction = match sort_key {
                SourcePickerSortKey::Name => SourcePickerSortDirection::Ascending,
                SourcePickerSortKey::Modified => SourcePickerSortDirection::Descending,
            };
        }

        self.sort_source_picker_entries();
        self.source_picker
            .entry_scroll
            .scroll_to_item(0, ScrollStrategy::Top);
        self.source_picker.error_message = None;
    }

    /// 设置选择器路径输入框聚焦状态。
    pub fn set_source_picker_path_input_focused(&mut self, is_focused: bool) {
        self.source_picker.is_path_input_focused = is_focused;
        if !is_focused {
            self.source_picker.path_input_selection_drag = None;
        }
    }

    /// 处理选择器路径输入框键盘输入。
    pub fn handle_source_picker_path_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        match keystroke.key.as_str() {
            "enter" => {
                let path = PathBuf::from(self.source_picker.path_input.trim());
                self.navigate_source_picker(path, cx);
            }
            "escape" => self.close_source_picker(),
            "backspace" => self.delete_source_picker_path_input_backward(),
            "delete" => self.delete_source_picker_path_input_forward(),
            "left" => self.move_source_picker_path_cursor_left(),
            "right" => self.move_source_picker_path_cursor_right(),
            "home" => self.move_source_picker_path_cursor_to_start(),
            "end" => self.move_source_picker_path_cursor_to_end(),
            _ => {
                if let Some(key_char) = keystroke.key_char.as_ref()
                    && !keystroke.modifiers.platform
                    && !key_char.chars().any(char::is_control)
                {
                    self.insert_source_picker_path_text(key_char);
                }
            }
        }
    }

    /// 开始路径输入框鼠标选择。
    pub fn begin_source_picker_path_pointer_selection(
        &mut self,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        let text_len = character_count(&self.source_picker.path_input);
        let character_index = character_index.min(text_len);
        let range = match granularity {
            TextSelectionGranularity::Character => character_index..character_index,
            TextSelectionGranularity::Word => {
                word_range_at(&self.source_picker.path_input, character_index)
                    .unwrap_or(character_index..character_index)
            }
            TextSelectionGranularity::Line => 0..text_len,
        };

        self.source_picker.path_input_cursor = range.end;
        self.source_picker.path_input_selection_anchor = Some(range.start);
        self.source_picker.path_input_selection_drag = Some(InputTextSelectionDrag {
            anchor_range: range,
            granularity,
        });
        self.source_picker.is_path_input_focused = true;
    }

    /// 更新路径输入框鼠标拖拽选择。
    pub fn update_source_picker_path_pointer_selection(&mut self, character_index: usize) {
        let Some(selection_drag) = self.source_picker.path_input_selection_drag.clone() else {
            return;
        };

        let text_len = character_count(&self.source_picker.path_input);
        let character_index = character_index.min(text_len);
        let next_range = match selection_drag.granularity {
            TextSelectionGranularity::Character => character_index..character_index,
            TextSelectionGranularity::Word => {
                word_range_at(&self.source_picker.path_input, character_index)
                    .unwrap_or(character_index..character_index)
            }
            TextSelectionGranularity::Line => 0..text_len,
        };
        let start = selection_drag.anchor_range.start.min(next_range.start);
        let end = selection_drag.anchor_range.end.max(next_range.end);

        self.source_picker.path_input_selection_anchor = Some(start);
        self.source_picker.path_input_cursor = end;
    }

    /// 结束路径输入框鼠标拖拽选择。
    pub fn finish_source_picker_path_pointer_selection(&mut self) {
        self.source_picker.path_input_selection_drag = None;
    }

    /// 切换待加载路径列表中的路径。
    fn toggle_source_picker_path(&mut self, path: PathBuf) {
        if let Some(index) = self
            .source_picker
            .selected_paths
            .iter()
            .position(|selected| selected == &path)
        {
            self.source_picker.selected_paths.remove(index);
            self.source_picker.error_message = None;
            return;
        }

        self.source_picker.selected_paths.push(path);
        self.source_picker.error_message = None;
    }

    /// 按当前排序设置重排选择器目录条目。
    fn sort_source_picker_entries(&mut self) {
        let sort_key = self.source_picker.sort_key;
        let sort_direction = self.source_picker.sort_direction;

        self.source_picker.entries.sort_by(|left, right| {
            compare_source_picker_entries(left, right, sort_key, sort_direction)
        });
    }

    /// 插入路径输入框文本，优先替换当前选区。
    fn insert_source_picker_path_text(&mut self, text: &str) {
        self.replace_source_picker_path_selection(text);
    }

    /// 向后删除一个字符或当前选区。
    fn delete_source_picker_path_input_backward(&mut self) {
        if self.source_picker.path_input_selection_range().is_some() {
            self.replace_source_picker_path_selection("");
            return;
        }
        if self.source_picker.path_input_cursor == 0 {
            return;
        }

        let cursor = self.source_picker.path_input_cursor;
        self.source_picker.path_input =
            remove_character_range(&self.source_picker.path_input, cursor - 1..cursor);
        self.source_picker.path_input_cursor -= 1;
    }

    /// 向前删除一个字符或当前选区。
    fn delete_source_picker_path_input_forward(&mut self) {
        if self.source_picker.path_input_selection_range().is_some() {
            self.replace_source_picker_path_selection("");
            return;
        }

        let cursor = self.source_picker.path_input_cursor;
        let text_len = character_count(&self.source_picker.path_input);
        if cursor >= text_len {
            return;
        }
        self.source_picker.path_input =
            remove_character_range(&self.source_picker.path_input, cursor..cursor + 1);
    }

    /// 将路径输入框光标左移一位，并清除选区。
    fn move_source_picker_path_cursor_left(&mut self) {
        self.source_picker.path_input_cursor =
            self.source_picker.path_input_cursor.saturating_sub(1);
        self.source_picker.path_input_selection_anchor = None;
    }

    /// 将路径输入框光标右移一位，并清除选区。
    fn move_source_picker_path_cursor_right(&mut self) {
        let text_len = character_count(&self.source_picker.path_input);
        self.source_picker.path_input_cursor =
            (self.source_picker.path_input_cursor + 1).min(text_len);
        self.source_picker.path_input_selection_anchor = None;
    }

    /// 将路径输入框光标移动到开头。
    fn move_source_picker_path_cursor_to_start(&mut self) {
        self.source_picker.path_input_cursor = 0;
        self.source_picker.path_input_selection_anchor = None;
    }

    /// 将路径输入框光标移动到末尾。
    fn move_source_picker_path_cursor_to_end(&mut self) {
        self.source_picker.path_input_cursor = character_count(&self.source_picker.path_input);
        self.source_picker.path_input_selection_anchor = None;
    }

    /// 替换路径输入框选区或在光标处插入文本。
    fn replace_source_picker_path_selection(&mut self, replacement: &str) {
        let selection_range = self
            .source_picker
            .path_input_selection_range()
            .unwrap_or(self.source_picker.path_input_cursor..self.source_picker.path_input_cursor);
        let mut next_text =
            remove_character_range(&self.source_picker.path_input, selection_range.clone());
        next_text = insert_text_at_character_index(&next_text, selection_range.start, replacement);

        self.source_picker.path_input = next_text;
        self.source_picker.path_input_cursor = selection_range.start + character_count(replacement);
        self.source_picker.path_input_selection_anchor = None;
    }
}

/// 比较两个选择器条目；名称排序保留目录优先，修改日期排序按时间全局排列。
fn compare_source_picker_entries(
    left: &BrowseEntry,
    right: &BrowseEntry,
    sort_key: SourcePickerSortKey,
    sort_direction: SourcePickerSortDirection,
) -> Ordering {
    match sort_key {
        SourcePickerSortKey::Name => compare_entry_groups(left, right)
            .then_with(|| compare_entry_names(left, right, sort_direction)),
        SourcePickerSortKey::Modified => compare_entry_modified(left, right, sort_direction)
            .then_with(|| compare_entry_groups(left, right))
            .then_with(|| compare_entry_names(left, right, SourcePickerSortDirection::Ascending)),
    }
}

/// 比较条目分组；目录排在普通文件和压缩包之前。
fn compare_entry_groups(left: &BrowseEntry, right: &BrowseEntry) -> Ordering {
    let group_index = |entry: &BrowseEntry| {
        if matches!(entry.kind, crate::loader::BrowseEntryKind::Directory) {
            0
        } else {
            1
        }
    };

    group_index(left).cmp(&group_index(right))
}

/// 比较条目名称；大小写不敏感比较优先，原始名称作为稳定兜底。
fn compare_entry_names(
    left: &BrowseEntry,
    right: &BrowseEntry,
    sort_direction: SourcePickerSortDirection,
) -> Ordering {
    let ordering = left
        .name
        .to_lowercase()
        .cmp(&right.name.to_lowercase())
        .then_with(|| left.name.cmp(&right.name));

    match sort_direction {
        SourcePickerSortDirection::Ascending => ordering,
        SourcePickerSortDirection::Descending => ordering.reverse(),
    }
}

/// 比较条目修改时间；缺少修改时间的条目始终放在同组末尾。
fn compare_entry_modified(
    left: &BrowseEntry,
    right: &BrowseEntry,
    sort_direction: SourcePickerSortDirection,
) -> Ordering {
    match (left.modified, right.modified) {
        (Some(left_modified), Some(right_modified)) => match sort_direction {
            SourcePickerSortDirection::Ascending => left_modified.cmp(&right_modified),
            SourcePickerSortDirection::Descending => right_modified.cmp(&left_modified),
        },
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigManager;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, UNIX_EPOCH};

    /// 测试配置路径计数器，避免污染真实用户配置。
    static NEXT_TEST_CONFIG_ID: AtomicUsize = AtomicUsize::new(0);

    /// 创建仅用于选择器状态测试的应用。
    fn test_app() -> ArgusApp {
        let id = NEXT_TEST_CONFIG_ID.fetch_add(1, Ordering::Relaxed);
        let config_dir = std::env::temp_dir().join(format!(
            "argus-source-picker-app-test-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&config_dir);
        ArgusApp::new_with_config_manager(ConfigManager::new(config_dir.join("settings.toml")))
    }

    /// 创建选择器排序测试使用的文件条目。
    fn test_entry(
        name: &str,
        kind: crate::loader::BrowseEntryKind,
        modified_offset: Option<u64>,
    ) -> BrowseEntry {
        BrowseEntry {
            path: PathBuf::from(format!("/tmp/{name}")),
            name: name.to_string(),
            kind,
            size: Some(1),
            modified: modified_offset.map(|offset| UNIX_EPOCH + Duration::from_secs(offset)),
            is_selectable: true,
            disabled_reason: None,
        }
    }

    /// 验证目录行单击会加入路径，再次点击会取消选择。
    #[test]
    fn selecting_directory_row_toggles_selected_path() {
        let mut app = test_app();
        let directory = PathBuf::from("/tmp/argus-directory-row");

        app.toggle_source_picker_directory(directory.clone());
        assert!(app.source_picker.is_selected(&directory));

        app.toggle_source_picker_directory(directory.clone());
        assert!(!app.source_picker.is_selected(&directory));
    }

    /// 验证文件条目只有可选时才能加入待加载列表。
    #[test]
    fn toggle_source_picker_file_respects_entry_selectability() {
        let mut app = test_app();
        let selectable = PathBuf::from("/tmp/app.log");
        let disabled = PathBuf::from("/tmp/folder");
        app.source_picker.entries = vec![
            BrowseEntry {
                path: selectable.clone(),
                name: "app.log".to_string(),
                kind: crate::loader::BrowseEntryKind::LogFile,
                size: Some(1),
                modified: None,
                is_selectable: true,
                disabled_reason: None,
            },
            BrowseEntry {
                path: disabled.clone(),
                name: "folder".to_string(),
                kind: crate::loader::BrowseEntryKind::Directory,
                size: None,
                modified: None,
                is_selectable: false,
                disabled_reason: Some("目录通过单击行选择，双击进入".to_string()),
            },
        ];

        app.toggle_source_picker_file(selectable.clone());
        app.toggle_source_picker_file(disabled);

        assert_eq!(app.source_picker.selected_paths, vec![selectable]);
        assert!(app.source_picker.error_message.is_some());
    }

    /// 验证取消选择器会清空临时待加载路径，避免下次打开误加载旧选择。
    #[test]
    fn close_source_picker_clears_pending_selection() {
        let mut app = test_app();
        app.source_picker.is_open = true;
        app.source_picker.selected_paths = vec![PathBuf::from("/tmp/app.log")];

        app.close_source_picker();

        assert!(!app.source_picker.is_open);
        assert!(app.source_picker.selected_paths.is_empty());
    }

    /// 验证名称排序在目录优先的前提下按名称升降序切换。
    #[test]
    fn source_picker_sort_by_name_keeps_directories_first() {
        let mut app = test_app();
        app.source_picker.entries = vec![
            test_entry("z.log", crate::loader::BrowseEntryKind::LogFile, Some(10)),
            test_entry("beta", crate::loader::BrowseEntryKind::Directory, Some(20)),
            test_entry("a.log", crate::loader::BrowseEntryKind::LogFile, Some(30)),
            test_entry("alpha", crate::loader::BrowseEntryKind::Directory, Some(40)),
        ];

        app.set_source_picker_sort(SourcePickerSortKey::Name);
        let ascending_names = app
            .source_picker
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ascending_names, vec!["alpha", "beta", "a.log", "z.log"]);

        app.set_source_picker_sort(SourcePickerSortKey::Name);
        let descending_names = app
            .source_picker
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(descending_names, vec!["beta", "alpha", "z.log", "a.log"]);
    }

    /// 验证修改日期排序默认按新到旧排列，缺失日期的条目始终靠后。
    #[test]
    fn source_picker_sort_by_modified_keeps_unknown_time_last() {
        let mut app = test_app();
        app.source_picker.entries = vec![
            test_entry("old.log", crate::loader::BrowseEntryKind::LogFile, Some(10)),
            test_entry(
                "old-dir",
                crate::loader::BrowseEntryKind::Directory,
                Some(20),
            ),
            test_entry("missing.log", crate::loader::BrowseEntryKind::LogFile, None),
            test_entry("new.log", crate::loader::BrowseEntryKind::LogFile, Some(30)),
        ];
        app.source_picker.sort_key = SourcePickerSortKey::Name;
        app.source_picker.sort_direction = SourcePickerSortDirection::Ascending;

        app.set_source_picker_sort(SourcePickerSortKey::Modified);

        let names = app
            .source_picker
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["new.log", "old-dir", "old.log", "missing.log"]);
    }
}
