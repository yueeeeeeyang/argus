//! 文件职责：渲染带语法高亮的远程文件只读预览独立窗口。
//! 创建日期：2026-07-03
//! 修改日期：2026-09-24
//! 作者：Argus 开发团队
//! 主要功能：以项目统一编辑器样式展示远程文本、代码高亮、行号，并支持文本拖拽选中、自动滚动、复制与全选。

use std::borrow::Borrow;
use std::ops::Range;

#[cfg(not(target_os = "windows"))]
use gpui::WindowControlArea;
use gpui::{
    AnyElement, ClipboardItem, Context, Entity, FocusHandle, FontWeight, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, SharedString,
    StyledText, Subscription, TextRun, UniformListScrollHandle, Window, canvas, div, point,
    prelude::*, px, rgb, uniform_list,
};

use crate::app::{ArgusApp, log_viewer_line_number_width, observe_app_theme};
use crate::fonts::{ARGUS_LOG_FONT_FAMILY, ARGUS_UI_FONT_FAMILY};
use crate::highlight::{
    HighlightCache, HighlightLanguage, HighlightSpan, detect_highlight_language,
};
use crate::infra::selection_autoscroll::{
    advance_negative_scroll, selection_autoscroll_intensity, selection_autoscroll_step_px,
};
use crate::infra::text_selection::{
    TextSelectionGranularity, byte_index_for_character, char_column_for_byte_index,
    character_count, slice_character_range, word_range_at,
};
use crate::platform::custom_titlebar;
use crate::remote::remote_file::FilePreviewContent;
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::custom_title_bar::{TITLE_BAR_HEIGHT, platform_window_controls};
use crate::ui::log_content_view::merge_syntax_and_selection_highlights;
use crate::ui::main_window::{
    WINDOW_CONTENT_INSET, WINDOW_CONTENT_PADDING, WINDOW_CONTENT_RADIUS,
    WINDOW_CONTENT_RING_ALLOWANCE, window_content_shadows,
};

/// 预览正文行高，保持与日志阅读区一致的高密度展示。
const FILE_PREVIEW_ROW_HEIGHT: f32 = 20.0;
/// 预览正文字号。
const FILE_PREVIEW_FONT_SIZE: f32 = 12.0;
/// 预览正文文本左内边距，与 `render_preview_line` 文本容器 `px_3()` 保持一致，供指针坐标换算字符列时扣除。
const FILE_PREVIEW_TEXT_LEFT_PADDING: f32 = 12.0;
/// 预览正文下方只读状态栏高度。
const FILE_PREVIEW_STATUS_BAR_HEIGHT: f32 = 26.0;
/// 居中状态卡片最大宽度，避免错误详情横向撑满窗口。
const FILE_PREVIEW_MESSAGE_MAX_WIDTH: f32 = 520.0;

/// 预览窗口正文状态，由读取回传的内容派生。
enum FilePreviewBody {
    /// 文本内容，按行拆分；`truncated` 表示因超过读取上限被截断。
    Text {
        /// 按行拆分后的文本。
        lines: Vec<String>,
        /// 是否因超过预览读取上限被截断。
        truncated: bool,
    },
    /// 二进制文件，无法以文本预览。
    Binary,
    /// 读取失败时携带的用户可读错误。
    Error(String),
}

/// 预览正文中的字符位置。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PreviewTextPosition {
    /// 0 基内容行号。
    line: usize,
    /// 行内字符列。
    column: usize,
}

/// 预览正文文本选区。
#[derive(Clone, Debug, Eq, PartialEq)]
struct PreviewTextSelection {
    /// 鼠标按下时的锚点。
    anchor: PreviewTextPosition,
    /// 当前拖拽到的焦点。
    focus: PreviewTextPosition,
}

impl PreviewTextSelection {
    /// 返回按文档顺序排列的选区端点。
    fn normalized(&self) -> (PreviewTextPosition, PreviewTextPosition) {
        if preview_text_position_le(self.anchor, self.focus) {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    /// 返回选区是否没有覆盖任何字符。
    fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }
}

/// 预览正文拖拽选择状态。
#[derive(Clone, Debug, Eq, PartialEq)]
struct PreviewTextSelectionDrag {
    /// 开始拖拽时按点击次数得到的锚点范围。
    anchor_range: PreviewTextSelection,
    /// 本次拖拽的选择粒度。
    granularity: TextSelectionGranularity,
}

/// 远程文件预览独立窗口视图。
pub(crate) struct FilePreviewWindow {
    /// 当前窗口使用的主题快照。
    theme: AppTheme,
    /// 文件名，用于标题展示。
    file_name: String,
    /// 根据文件名识别的语法语言，供标题标签和逐行高亮共同使用。
    language: HighlightLanguage,
    /// 预览正文状态。
    body: FilePreviewBody,
    /// 正文滚动句柄。
    scroll: UniformListScrollHandle,
    /// 可见行语法高亮缓存，避免滚动或主题刷新时反复扫描相同代码行。
    highlight_cache: HighlightCache,
    /// 窗口根元素焦点句柄，用于接收键盘事件并稳定焦点归属。
    root_focus: FocusHandle,
    /// 当前正文选区；行号栏不参与选中。
    preview_selection: Option<PreviewTextSelection>,
    /// 当前正文拖拽选择状态。
    preview_selection_drag: Option<PreviewTextSelectionDrag>,
    /// 拖拽选择自动滚动的最近指针位置；为空表示当前没有进行中的拖拽。
    selection_autoscroll_pointer: Option<Point<Pixels>>,
    /// 自动滚动逐帧循环是否已启动，避免重复调度。
    selection_autoscroll_loop_active: bool,
    /// 主应用状态订阅，主题切换后窗口跟随刷新。
    _app_observer: Subscription,
}

impl FilePreviewWindow {
    /// 创建远程文件预览窗口。
    ///
    /// 参数说明：
    /// - `app`：主应用实体。
    /// - `theme`：首次绘制使用的主题。
    /// - `file_name`：预览文件名。
    /// - `content`：worker 读取回传的预览内容。
    /// - `cx`：窗口上下文，用于创建滚动句柄和订阅主应用变化。
    pub(crate) fn new(
        app: Entity<ArgusApp>,
        theme: AppTheme,
        file_name: String,
        content: FilePreviewContent,
        cx: &mut Context<Self>,
    ) -> Self {
        let body = match content {
            FilePreviewContent::Text { content, truncated } => {
                // 使用 `str::lines` 正确处理 `\r\n`/`\n` 换行，且不产生末尾空行。
                let lines = content.lines().map(str::to_string).collect();
                FilePreviewBody::Text { lines, truncated }
            }
            FilePreviewContent::Binary => FilePreviewBody::Binary,
            FilePreviewContent::Error(message) => FilePreviewBody::Error(message),
        };
        let language = detect_highlight_language(&file_name, &file_name);
        let _app_observer = observe_app_theme(cx, &app, theme.clone(), |view, theme, _| {
            view.theme = theme.clone();
        });

        Self {
            theme,
            file_name,
            language,
            body,
            scroll: UniformListScrollHandle::new(),
            highlight_cache: HighlightCache::default(),
            root_focus: cx.focus_handle(),
            preview_selection: None,
            preview_selection_drag: None,
            selection_autoscroll_pointer: None,
            selection_autoscroll_loop_active: false,
            _app_observer,
        }
    }

    /// 返回当前正文文本行；非文本正文返回 `None`。
    fn text_lines(&self) -> Option<&[String]> {
        match &self.body {
            FilePreviewBody::Text { lines, .. } => Some(lines),
            _ => None,
        }
    }

    /// 复制当前正文选区到系统剪贴板；没有选区时不执行复制。
    fn copy_preview_selection(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.selected_preview_text() else {
            return;
        };
        let app_context: &gpui::App = (*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// 选中全部正文文本；空文档不改变状态。
    fn select_all_preview_text(&mut self) {
        let Some(lines) = self.text_lines() else {
            return;
        };
        if let Some(selection) = preview_select_all_selection(lines) {
            self.preview_selection = Some(selection);
        }
    }

    /// 清除正文选区与拖拽、自动滚动状态；Escape 键与空选区释放时调用。
    fn clear_preview_selection(&mut self) {
        self.preview_selection = None;
        self.preview_selection_drag = None;
        self.selection_autoscroll_pointer = None;
    }

    /// 返回当前正文选中的文本，跨行片段用 `\n` 连接；无有效选区时返回 `None`。
    fn selected_preview_text(&self) -> Option<String> {
        let selection = self.preview_selection.as_ref()?;
        if selection.is_empty() {
            return None;
        }
        let lines = self.text_lines()?;
        selected_preview_text_from_lines(lines, selection)
    }

    /// 根据鼠标位置开始正文选择，并按点击次数确定选择粒度。
    fn begin_preview_text_selection(
        &mut self,
        line: usize,
        line_text: &str,
        pointer_x: Pixels,
        click_count: usize,
        window: &mut Window,
    ) {
        self.root_focus.focus(window);
        let position = self.preview_text_position_from_pointer(line, line_text, pointer_x, window);
        let granularity = preview_text_granularity_for_click_count(click_count);
        let anchor_range =
            preview_text_range_for_granularity(line, line_text, position.column, granularity);
        self.preview_selection = Some(anchor_range.clone());
        self.preview_selection_drag = Some(PreviewTextSelectionDrag {
            anchor_range,
            granularity,
        });
    }

    /// 拖拽过程中更新正文选择；词/行粒度按锚点粒度扩展。
    fn update_preview_text_selection(
        &mut self,
        line: usize,
        line_text: &str,
        pointer_x: Pixels,
        window: &mut Window,
    ) {
        let Some(drag) = self.preview_selection_drag.clone() else {
            return;
        };
        let position = self.preview_text_position_from_pointer(line, line_text, pointer_x, window);
        let focus_range =
            preview_text_range_for_granularity(line, line_text, position.column, drag.granularity);
        self.preview_selection = Some(merge_preview_text_ranges(&drag.anchor_range, &focus_range));
    }

    /// 结束正文选择；没有选中字符时清理选区。
    fn finish_preview_text_selection(&mut self) {
        self.preview_selection_drag = None;
        self.selection_autoscroll_pointer = None;
        if self
            .preview_selection
            .as_ref()
            .is_some_and(PreviewTextSelection::is_empty)
        {
            self.preview_selection = None;
        }
    }

    /// 记录拖拽选择指针位置；指针进入视口纵向边缘区时启动逐帧自动滚动循环。
    ///
    /// 说明：GPUI 按命中测试分发鼠标事件，行元素的 `on_mouse_move` 在指针离开行后不再
    /// 触发，这里通过窗口级监听把指针位置持续喂给自动滚动循环。
    ///
    /// 返回值：本次调用新启动了自动滚动循环时返回 `true`。
    fn track_selection_autoscroll_pointer(
        &mut self,
        pointer: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.preview_selection_drag.is_none() {
            return false;
        }
        self.selection_autoscroll_pointer = Some(pointer);
        if self.selection_autoscroll_loop_active
            || !self.pointer_in_selection_autoscroll_zone(pointer)
        {
            return false;
        }

        self.selection_autoscroll_loop_active = true;
        schedule_selection_autoscroll_frame(cx.entity(), window);
        true
    }

    /// 执行一帧自动滚动，并把选区扩展到指针钳制在视口内后对应的行列。
    ///
    /// 说明：预览列表只支持纵向滚动；滚动后按当前偏移反算指针所在正文行，
    /// 使选区随滚动逐帧向不可见区域扩展。
    ///
    /// 返回值：拖拽仍在进行且指针停留在纵向边缘滚动区时返回 `true`，表示继续调度下一帧。
    fn step_selection_autoscroll(&mut self, window: &mut Window) -> bool {
        let Some(pointer) = self.selection_autoscroll_pointer else {
            return false;
        };
        if self.preview_selection_drag.is_none() {
            self.selection_autoscroll_pointer = None;
            return false;
        }
        let scroll_state = self.scroll.0.as_ref().borrow();
        let base_handle = scroll_state.base_handle.clone();
        let content_size = scroll_state.last_item_size.map(|size| size.contents);
        drop(scroll_state);
        let bounds = base_handle.bounds();
        if bounds.size.height <= px(0.0) || bounds.size.width <= px(0.0) {
            self.selection_autoscroll_pointer = None;
            return false;
        }
        let intensity = selection_autoscroll_intensity(
            f32::from(pointer.y),
            f32::from(bounds.top()),
            f32::from(bounds.bottom()),
        );
        if intensity == 0.0 {
            return false;
        }

        let max_vertical = content_size
            .map(|size| (size.height - bounds.size.height).max(px(0.0)))
            .unwrap_or(px(0.0));
        let current_offset = base_handle.offset();
        let next_offset_y = advance_negative_scroll(
            f32::from(current_offset.y),
            selection_autoscroll_step_px(intensity),
            f32::from(max_vertical),
        );
        base_handle.set_offset(point(current_offset.x, px(next_offset_y)));

        let Some(line_count) = self.text_lines().map(<[String]>::len) else {
            self.selection_autoscroll_pointer = None;
            return false;
        };
        if line_count == 0 {
            self.selection_autoscroll_pointer = None;
            return false;
        }
        // 指针钳制在视口内，把内容坐标反算成 0 基行号并夹到当前行数范围内。
        let clamped_y = pointer.y.clamp(bounds.top(), bounds.bottom());
        let content_y = f32::from(clamped_y - bounds.top()) - next_offset_y;
        let line =
            ((content_y / FILE_PREVIEW_ROW_HEIGHT).floor().max(0.0) as usize).min(line_count - 1);
        let Some(line_text) = self.text_lines().and_then(|lines| lines.get(line)).cloned() else {
            self.selection_autoscroll_pointer = None;
            return false;
        };
        let clamped_x = pointer.x.clamp(bounds.left(), bounds.right());
        self.update_preview_text_selection(line, &line_text, clamped_x, window);
        true
    }

    /// 判断指针是否位于正文视口纵向自动滚动边缘区。
    fn pointer_in_selection_autoscroll_zone(&self, pointer: Point<Pixels>) -> bool {
        let bounds = self.scroll.0.as_ref().borrow().base_handle.bounds();
        if bounds.size.height <= px(0.0) {
            return false;
        }
        selection_autoscroll_intensity(
            f32::from(pointer.y),
            f32::from(bounds.top()),
            f32::from(bounds.bottom()),
        ) != 0.0
    }

    /// 根据鼠标横坐标计算正文行内字符列；行号栏宽度与文本左内边距不参与命中。
    ///
    /// 说明：与线程详情窗口保持一致，使用 GPUI 文本系统 shaping 结果按 x 坐标找最近
    /// 字节下标，再换算成字符列，避免等宽折算在多字节字符处偏移。
    fn preview_text_position_from_pointer(
        &self,
        line: usize,
        line_text: &str,
        pointer_x: Pixels,
        window: &mut Window,
    ) -> PreviewTextPosition {
        let scroll_state = self.scroll.0.as_ref().borrow();
        let bounds = scroll_state.base_handle.bounds();
        let horizontal_offset = scroll_state.base_handle.offset().x;
        drop(scroll_state);
        let line_count = self.text_lines().map(<[String]>::len).unwrap_or(0);
        let text_relative_x = pointer_x
            - bounds.left()
            - horizontal_offset
            - px(log_viewer_line_number_width(line_count) + FILE_PREVIEW_TEXT_LEFT_PADDING);
        if line_text.is_empty() || text_relative_x <= px(0.0) {
            return PreviewTextPosition { line, column: 0 };
        }

        let mut text_style = window.text_style();
        text_style.font_family = ARGUS_LOG_FONT_FAMILY.into();
        text_style.font_size = px(FILE_PREVIEW_FONT_SIZE).into();
        let run = TextRun {
            len: line_text.len(),
            font: text_style.font(),
            color: text_style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let shaped_line = window.text_system().shape_line(
            SharedString::from(line_text.to_string()),
            text_style.font_size.to_pixels(window.rem_size()),
            &[run],
            None,
        );
        let byte_index = shaped_line.closest_index_for_x(text_relative_x);
        PreviewTextPosition {
            line,
            column: char_column_for_byte_index(line_text, byte_index),
        }
    }

    /// 渲染预览窗口标题栏：平台窗口控件（macOS 原生红绿灯占位）、文件图标与文件名、拖拽空白。
    ///
    /// 关闭/最小化/最大化由系统红绿灯承担，不再提供右侧自定义关闭按钮；
    /// 标题栏骨架（高度、底色、拖拽与双击缩放）与主窗口自定义标题栏保持一致。
    fn render_header(&self, window: &Window) -> impl IntoElement {
        let theme = self.theme.clone();
        let is_maximized = window.is_maximized();

        div()
            .id("file-preview-title-bar")
            .h(px(TITLE_BAR_HEIGHT))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .bg(rgb(theme.title_bar))
            .occlude()
            .child(platform_window_controls(is_maximized, &theme))
            .child(
                // 与主标题栏按钮组保持同一节奏，避开红绿灯右缘；标题栏只保留文件名称。
                div()
                    .pl(px(8.0))
                    .flex_none()
                    .text_size(px(13.0))
                    .line_height(px(18.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(theme.foreground))
                    .child(self.file_name.clone()),
            )
            .child(preview_title_drag_area())
    }

    /// 渲染底部只读状态栏；作为窗口级底栏直接铺在黑色背板上，文字与玻璃面板内容左对齐。
    fn render_status_bar(&self) -> impl IntoElement {
        let content_status = match &self.body {
            FilePreviewBody::Text { lines, truncated } => {
                if *truncated {
                    format!("{} 行  ·  内容已截断", lines.len())
                } else {
                    format!("{} 行", lines.len())
                }
            }
            FilePreviewBody::Binary => "二进制文件".to_string(),
            FilePreviewBody::Error(_) => "读取失败".to_string(),
        };
        div()
            .h(px(FILE_PREVIEW_STATUS_BAR_HEIGHT))
            .flex_none()
            .px(px(WINDOW_CONTENT_INSET + 12.0))
            .flex()
            .items_center()
            .justify_between()
            .bg(rgb(self.theme.background))
            .text_size(px(11.0))
            .text_color(rgb(self.theme.foreground_muted))
            .child("只读预览")
            .child(format!(
                "UTF-8  ·  {}  ·  {content_status}",
                self.language.display_name()
            ))
    }

    /// 渲染预览正文。
    fn render_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        match &self.body {
            FilePreviewBody::Text { lines, .. } => {
                // 空文件不应创建 0 行虚拟列表；部分平台在首次布局时会为其生成无效可见区间。
                if lines.is_empty() {
                    return render_preview_message(
                        ArgusIcon::FileText,
                        "文件内容为空",
                        "该文件没有可显示的文本内容。",
                        false,
                        &self.theme,
                    );
                }
                let line_count = lines.len();
                // 行号栏宽度随行数自适应（复用日志阅读区算法），避免固定宽度在行号过多时截断。
                let line_number_width = log_viewer_line_number_width(line_count);
                div()
                    .size_full()
                    .relative()
                    .bg(rgb(self.theme.content))
                    // 行号栏与日志阅读区同一风格：内容底色、无独立色块和分隔线。
                    .child(
                        uniform_list(
                            "file-preview-lines",
                            line_count,
                            cx.processor(move |this, range: Range<usize>, _window, cx| {
                                // 直接通过 `this` 访问正文与主题，避免每帧深拷贝整个行向量。
                                let FilePreviewBody::Text { lines, .. } = &this.body else {
                                    return Vec::new();
                                };
                                // 窗口初始化、缩放或关闭过程中，框架可能传入基于旧布局的区间。
                                // 先夹到当前行数，避免直接切片越界导致整个应用 panic 退出。
                                let visible_range = clamp_preview_line_range(range, lines.len());
                                let start = visible_range.start;
                                lines[visible_range]
                                    .iter()
                                    .enumerate()
                                    .map(|(offset, line)| {
                                        let line_number = start + offset + 1;
                                        let line_index = line_number - 1;
                                        let syntax_spans = this.highlight_cache.highlight_line(
                                            line_index,
                                            this.language,
                                            line,
                                        );
                                        let selection_range = preview_selection_byte_range_for_line(
                                            this.preview_selection.as_ref(),
                                            line_index,
                                            line,
                                        );
                                        render_preview_line(
                                            line_number,
                                            line,
                                            line_number_width,
                                            syntax_spans,
                                            selection_range,
                                            &this.theme,
                                            cx,
                                        )
                                        .into_any_element()
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .size_full()
                        .track_scroll(self.scroll.clone()),
                    )
                    .child(render_preview_selection_autoscroll_sensor(cx))
                    .into_any_element()
            }
            FilePreviewBody::Binary => render_preview_message(
                ArgusIcon::File,
                "无法预览二进制文件",
                "当前预览器仅支持文本内容，可返回文件列表后直接下载。",
                false,
                &self.theme,
            ),
            FilePreviewBody::Error(message) => {
                render_preview_message(ArgusIcon::Info, "文件预览失败", message, true, &self.theme)
            }
        }
    }
}

/// 将虚拟列表请求的行区间夹到当前文本边界内。
///
/// 参数说明：
/// - `range`：GPUI 根据视口估算的行区间。
/// - `line_count`：当前预览文本的实际行数。
///
/// 返回值：可安全用于行向量切片的升序区间；完全越界时返回末尾空区间。
fn clamp_preview_line_range(range: Range<usize>, line_count: usize) -> Range<usize> {
    let start = range.start.min(line_count);
    let end = range.end.min(line_count);
    start.min(end)..start.max(end)
}

impl Render for FilePreviewWindow {
    /// 渲染预览窗口主体：黑色背板 + 与主窗口一致的圆角玻璃板内容区。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root_focus_for_click = self.root_focus.clone();
        div()
            .id("file-preview-window-root")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(self.theme.background))
            .font_family(ARGUS_UI_FONT_FAMILY)
            .text_color(rgb(self.theme.foreground))
            .occlude()
            .focusable()
            .track_focus(&self.root_focus)
            .on_click(move |_, window, _| {
                root_focus_for_click.focus(window);
            })
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                let keystroke = &event.keystroke;
                let key = keystroke.key.to_lowercase();
                // 预览窗口独立于主应用，快捷键经根焦点处理；macOS 用 Cmd、Windows/Linux 用 Ctrl。
                if keystroke.modifiers.secondary() {
                    match key.as_str() {
                        "c" => {
                            cx.stop_propagation();
                            view.copy_preview_selection(cx);
                        }
                        "a" => {
                            cx.stop_propagation();
                            view.select_all_preview_text();
                            cx.notify();
                        }
                        _ => {}
                    }
                    return;
                }
                if key == "escape"
                    && (view.preview_selection.is_some() || view.preview_selection_drag.is_some())
                {
                    cx.stop_propagation();
                    view.clear_preview_selection();
                    cx.notify();
                }
            }))
            .child(self.render_header(window))
            .child(
                // 与主窗口同一套玻璃板结构：8px 窗口间距 + 圆角底板 + 多层投影，
                // 内部 8px 留白保证内容不盖住圆角（GPUI 裁剪只支持矩形）。
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .pt(px(WINDOW_CONTENT_RING_ALLOWANCE))
                    .pl(px(WINDOW_CONTENT_INSET))
                    .pr(px(WINDOW_CONTENT_INSET))
                    // 面板底边与状态栏之间收窄到 6px 窗口间距。
                    .pb(px(6.0))
                    .child(
                        div()
                            .size_full()
                            .p(px(WINDOW_CONTENT_PADDING))
                            .rounded(px(WINDOW_CONTENT_RADIUS))
                            .bg(rgb(self.theme.content))
                            .shadow(window_content_shadows())
                            .child(
                                div()
                                    .size_full()
                                    .flex()
                                    .flex_col()
                                    .overflow_hidden()
                                    .font_family(ARGUS_LOG_FONT_FAMILY)
                                    .child(self.render_body(cx)),
                            ),
                    ),
            )
            // 状态栏作为窗口级底栏直接显示在背板上，玻璃面板随之上移让位。
            .child(self.render_status_bar())
    }
}

/// 渲染预览标题栏的拖拽空白，支持拖动窗口与双击最大化；范式同主窗口标签栏拖拽区。
fn preview_title_drag_area() -> impl IntoElement {
    let drag_area = div().id("file-preview-title-drag-area").h_full().flex_1();
    // Windows 需要普通客户区事件调用显式窗口操作；其余平台保留 GPUI 控制区语义。
    #[cfg(not(target_os = "windows"))]
    let drag_area = drag_area.window_control_area(WindowControlArea::Drag);

    drag_area.on_mouse_down(
        MouseButton::Left,
        move |event: &MouseDownEvent, window, cx| {
            match event.click_count {
                1 => custom_titlebar::start_window_drag(window),
                2 => window.zoom_window(),
                _ => {}
            }
            cx.stop_propagation();
        },
    )
}

/// 渲染预览正文单行：行号 + 文本内容，并挂载文本拖拽选择的鼠标监听。
fn render_preview_line(
    line_number: usize,
    line: &str,
    line_number_width: f32,
    syntax_spans: Vec<HighlightSpan>,
    selection_range: Option<Range<usize>>,
    theme: &AppTheme,
    cx: &mut Context<FilePreviewWindow>,
) -> impl IntoElement {
    let line_index = line_number - 1;
    let text_element = render_highlighted_preview_text(line, syntax_spans, selection_range, theme);
    let line_for_mouse_down = line.to_string();
    let line_for_mouse_move = line.to_string();

    div()
        .id(SharedString::from(format!(
            "file-preview-line-{line_number}"
        )))
        .h(px(FILE_PREVIEW_ROW_HEIGHT))
        .w_full()
        .flex()
        .items_center()
        .text_size(px(FILE_PREVIEW_FONT_SIZE))
        .line_height(px(FILE_PREVIEW_ROW_HEIGHT))
        .bg(rgb(theme.content))
        .hover(|this| this.bg(rgb(theme.current_line)))
        .cursor_text()
        .child(
            div()
                .w(px(line_number_width))
                .h_full()
                .flex_none()
                .pr_3()
                .flex()
                .items_center()
                .justify_end()
                .bg(rgb(theme.content))
                .text_color(rgb(theme.foreground_muted))
                .child(line_number.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .px_3()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_color(rgb(theme.foreground))
                .child(text_element),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |view, event: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                view.begin_preview_text_selection(
                    line_index,
                    &line_for_mouse_down,
                    event.position.x,
                    event.click_count,
                    window,
                );
                cx.notify();
            }),
        )
        .on_mouse_move(
            cx.listener(move |view, event: &MouseMoveEvent, window, cx| {
                if !event.dragging() || view.preview_selection_drag.is_none() {
                    return;
                }
                cx.stop_propagation();
                view.update_preview_text_selection(
                    line_index,
                    &line_for_mouse_move,
                    event.position.x,
                    window,
                );
                cx.notify();
            }),
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |view, _, _, cx| {
                if view.preview_selection_drag.is_some() {
                    cx.stop_propagation();
                    view.finish_preview_text_selection();
                    cx.notify();
                }
            }),
        )
}

/// 把纯逻辑高亮范围转换为 GPUI 文本样式。
///
/// 参数说明：
/// - `line`：当前展示行。
/// - `spans`：高亮器生成的不重叠 UTF-8 字节范围。
/// - `selection_range`：当前行内被正文选区覆盖的 UTF-8 字节范围；为 `None` 时只渲染语法高亮。
/// - `theme`：当前窗口主题。
///
/// 返回值：没有任何高亮时返回普通文本，否则返回带主题色的 `StyledText`。
fn render_highlighted_preview_text(
    line: &str,
    spans: Vec<HighlightSpan>,
    selection_range: Option<Range<usize>>,
    theme: &AppTheme,
) -> AnyElement {
    // 复用日志阅读区的语法/选区合并实现：选区背景优先，未选中片段保留语法色。
    let highlights = merge_syntax_and_selection_highlights(spans, selection_range, theme);
    if highlights.is_empty() {
        return line.to_string().into_any_element();
    }
    StyledText::new(line.to_string())
        .with_highlights(highlights)
        .into_any_element()
}

/// 渲染空文件、二进制和失败状态的统一居中卡片。
fn render_preview_message(
    icon: ArgusIcon,
    title: &str,
    detail: &str,
    is_error: bool,
    theme: &AppTheme,
) -> AnyElement {
    let icon_color = if is_error {
        theme.error
    } else {
        theme.foreground_muted
    };
    div()
        .size_full()
        .p_6()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w_full()
                .max_w(px(FILE_PREVIEW_MESSAGE_MAX_WIDTH))
                .p_5()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .rounded_lg()
                .border_1()
                .border_color(rgb(theme.border))
                .bg(rgb(theme.current_line))
                .child(render_icon(icon, icon_color, 24.0))
                .child(
                    div()
                        .mt_1()
                        .text_size(px(13.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(if is_error {
                            theme.error
                        } else {
                            theme.foreground
                        }))
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .max_w_full()
                        .text_center()
                        .text_size(px(12.0))
                        .text_color(rgb(theme.foreground_muted))
                        .child(detail.to_string()),
                ),
        )
        .into_any_element()
}

/// 调度预览窗口拖拽选择自动滚动的下一帧；拖拽结束或指针离开边缘区时循环自动停止。
fn schedule_selection_autoscroll_frame(entity: Entity<FilePreviewWindow>, window: &mut Window) {
    window.on_next_frame(move |window, cx| {
        let keep_running = entity.update(cx, |view, _| view.step_selection_autoscroll(window));
        if keep_running {
            cx.notify(entity.entity_id());
            schedule_selection_autoscroll_frame(entity, window);
        } else {
            entity.update(cx, |view, _| {
                view.selection_autoscroll_loop_active = false;
            });
        }
    });
}

/// 渲染拖拽选择自动滚动传感器。
///
/// 说明：行元素的 `on_mouse_move` 在指针离开行后不再触发，这里通过 canvas 在绘制期注册
/// 窗口级鼠标监听，持续把指针位置喂给自动滚动循环；canvas 不参与命中测试，不影响行交互。
fn render_preview_selection_autoscroll_sensor(cx: &mut Context<FilePreviewWindow>) -> AnyElement {
    let entity = cx.entity();
    canvas(
        |_, _, _| (),
        move |_, _, window: &mut Window, _| {
            window.on_mouse_event({
                let entity = entity.clone();
                move |event: &MouseMoveEvent, phase, window, cx| {
                    if !phase.bubble() || !event.dragging() {
                        return;
                    }
                    let entity_id = entity.entity_id();
                    let started = entity.update(cx, |view, view_cx| {
                        view.track_selection_autoscroll_pointer(event.position, window, view_cx)
                    });
                    if started {
                        cx.notify(entity_id);
                    }
                }
            });

            // 指针在行外释放时行级监听收不到事件，这里兜底结束选择，避免拖拽状态悬挂。
            window.on_mouse_event({
                let entity = entity.clone();
                move |event: &MouseUpEvent, phase, _, cx| {
                    if !phase.bubble() || event.button != MouseButton::Left {
                        return;
                    }
                    let entity_id = entity.entity_id();
                    let handled = entity.update(cx, |view, _| {
                        let handled = view.preview_selection_drag.is_some();
                        if handled {
                            view.finish_preview_text_selection();
                        }
                        handled
                    });
                    if handled {
                        cx.notify(entity_id);
                    }
                }
            });
        },
    )
    .absolute()
    .size_full()
    .into_any_element()
}

/// 判断预览正文位置是否按文档顺序不晚于另一个位置。
fn preview_text_position_le(left: PreviewTextPosition, right: PreviewTextPosition) -> bool {
    left.line < right.line || (left.line == right.line && left.column <= right.column)
}

/// 根据鼠标点击次数选择正文的选择粒度。
fn preview_text_granularity_for_click_count(click_count: usize) -> TextSelectionGranularity {
    match click_count {
        0 | 1 => TextSelectionGranularity::Character,
        2 => TextSelectionGranularity::Word,
        _ => TextSelectionGranularity::Line,
    }
}

/// 按指定粒度把鼠标命中的正文位置扩展成可拖拽合并的选区范围。
fn preview_text_range_for_granularity(
    line: usize,
    line_text: &str,
    column: usize,
    granularity: TextSelectionGranularity,
) -> PreviewTextSelection {
    let character_count = character_count(line_text);
    let range = match granularity {
        TextSelectionGranularity::Character => {
            column.min(character_count)..column.min(character_count)
        }
        TextSelectionGranularity::Word => word_range_at(line_text, column)
            .unwrap_or_else(|| column.min(character_count)..column.min(character_count)),
        TextSelectionGranularity::Line => 0..character_count,
    };

    PreviewTextSelection {
        anchor: PreviewTextPosition {
            line,
            column: range.start,
        },
        focus: PreviewTextPosition {
            line,
            column: range.end,
        },
    }
}

/// 合并拖拽起点和当前命中范围，得到跨行或跨词的最终正文选区。
fn merge_preview_text_ranges(
    anchor_range: &PreviewTextSelection,
    focus_range: &PreviewTextSelection,
) -> PreviewTextSelection {
    let (anchor_start, anchor_end) = anchor_range.normalized();
    let (focus_start, focus_end) = focus_range.normalized();
    PreviewTextSelection {
        anchor: if preview_text_position_le(anchor_start, focus_start) {
            anchor_start
        } else {
            focus_start
        },
        focus: if preview_text_position_le(anchor_end, focus_end) {
            focus_end
        } else {
            anchor_end
        },
    }
}

/// 构造覆盖全部正文行的选区；空文档返回 `None`。
fn preview_select_all_selection(lines: &[String]) -> Option<PreviewTextSelection> {
    let last_line = lines.len().checked_sub(1)?;
    let last_column = character_count(&lines[last_line]);
    Some(PreviewTextSelection {
        anchor: PreviewTextPosition { line: 0, column: 0 },
        focus: PreviewTextPosition {
            line: last_line,
            column: last_column,
        },
    })
}

/// 计算当前行被正文选区覆盖的 UTF-8 字节范围，用于叠加选择背景；行号栏不参与。
fn preview_selection_byte_range_for_line(
    selection: Option<&PreviewTextSelection>,
    line: usize,
    line_text: &str,
) -> Option<Range<usize>> {
    let selection = selection?;
    let (start, end) = selection.normalized();
    if line < start.line || line > end.line {
        return None;
    }

    let line_character_count = character_count(line_text);
    let start_column = if line == start.line {
        start.column.min(line_character_count)
    } else {
        0
    };
    let end_column = if line == end.line {
        end.column.min(line_character_count)
    } else {
        line_character_count
    };
    (start_column < end_column).then(|| {
        byte_index_for_character(line_text, start_column)
            ..byte_index_for_character(line_text, end_column)
    })
}

/// 从正文行集合中提取当前选区文本，保留跨行换行符以便复制后仍可阅读。
fn selected_preview_text_from_lines(
    lines: &[String],
    selection: &PreviewTextSelection,
) -> Option<String> {
    if selection.is_empty() || lines.is_empty() {
        return None;
    }

    let (start, end) = selection.normalized();
    if start.line >= lines.len() {
        return None;
    }

    let end_line = end.line.min(lines.len().saturating_sub(1));
    let mut selected = String::new();
    for (line, text) in lines.iter().enumerate().take(end_line + 1).skip(start.line) {
        if line > start.line {
            selected.push('\n');
        }
        let line_character_count = character_count(text);
        let start_column = if line == start.line {
            start.column.min(line_character_count)
        } else {
            0
        };
        let end_column = if line == end.line {
            end.column.min(line_character_count)
        } else {
            line_character_count
        };
        if start_column < end_column {
            selected.push_str(&slice_character_range(text, start_column..end_column));
        }
    }

    (!selected.is_empty()).then_some(selected)
}

#[cfg(test)]
mod tests {
    use super::{
        PreviewTextPosition, PreviewTextSelection, clamp_preview_line_range,
        merge_preview_text_ranges, preview_select_all_selection,
        preview_selection_byte_range_for_line, preview_text_granularity_for_click_count,
        preview_text_range_for_granularity, selected_preview_text_from_lines,
    };
    use crate::infra::text_selection::TextSelectionGranularity;

    /// 构造正文字符位置。
    fn pos(line: usize, column: usize) -> PreviewTextPosition {
        PreviewTextPosition { line, column }
    }

    /// 构造正文选区。
    fn sel(anchor: PreviewTextPosition, focus: PreviewTextPosition) -> PreviewTextSelection {
        PreviewTextSelection { anchor, focus }
    }

    /// 正常可见区间不应被修改。
    #[test]
    fn preview_line_range_keeps_valid_bounds() {
        assert_eq!(clamp_preview_line_range(2..5, 8), 2..5);
    }

    /// 框架返回超过当前行数的旧区间时，应夹到向量末尾而非 panic。
    #[test]
    fn preview_line_range_clamps_stale_bounds() {
        assert_eq!(clamp_preview_line_range(3..12, 5), 3..5);
        assert_eq!(clamp_preview_line_range(8..12, 5), 5..5);
    }

    /// 即使异常区间的起点大于终点，也必须返回可安全切片的升序区间。
    #[test]
    fn preview_line_range_normalizes_reversed_bounds() {
        let reversed_range = std::ops::Range { start: 6, end: 2 };
        assert_eq!(clamp_preview_line_range(reversed_range, 8), 2..6);
    }

    /// 反向拖拽的选区归一化后应按文档顺序返回端点。
    #[test]
    fn preview_selection_normalizes_reversed_endpoints() {
        let reversed = sel(pos(3, 2), pos(1, 4));
        let (start, end) = reversed.normalized();
        assert_eq!(start, pos(1, 4));
        assert_eq!(end, pos(3, 2));
        assert!(!reversed.is_empty());
    }

    /// 点击次数与选择粒度的映射应与日志阅读区一致：单击字符、双击词、三击整行。
    #[test]
    fn preview_granularity_follows_click_count() {
        assert_eq!(
            preview_text_granularity_for_click_count(1),
            TextSelectionGranularity::Character
        );
        assert_eq!(
            preview_text_granularity_for_click_count(2),
            TextSelectionGranularity::Word
        );
        assert_eq!(
            preview_text_granularity_for_click_count(3),
            TextSelectionGranularity::Line
        );
    }

    /// 字符粒度按下只记录光标锚点，不主动扩展选区。
    #[test]
    fn character_granularity_keeps_empty_anchor_range() {
        let range = preview_text_range_for_granularity(
            0,
            "hello world",
            6,
            TextSelectionGranularity::Character,
        );
        assert!(range.is_empty());
        assert_eq!(range.anchor, pos(0, 6));
    }

    /// 词粒度双击命中词身时选中整词，点在空白处回退为空选区。
    #[test]
    fn word_granularity_selects_hit_word_and_ignores_whitespace() {
        let range =
            preview_text_range_for_granularity(1, "fn main() {", 4, TextSelectionGranularity::Word);
        assert_eq!((range.anchor, range.focus), (pos(1, 3), pos(1, 7)));

        let whitespace =
            preview_text_range_for_granularity(0, "ab cd", 2, TextSelectionGranularity::Word);
        assert!(whitespace.is_empty());
    }

    /// 行粒度三击选中整行，与指针落点无关。
    #[test]
    fn line_granularity_selects_entire_line() {
        let range =
            preview_text_range_for_granularity(2, "let x = 1;", 3, TextSelectionGranularity::Line);
        assert_eq!((range.anchor, range.focus), (pos(2, 0), pos(2, 10)));
    }

    /// 拖拽合并时选区应从锚点起点扩展到焦点终点，允许跨行。
    #[test]
    fn merge_preview_text_ranges_spans_from_anchor_to_focus() {
        let anchor_range = sel(pos(0, 3), pos(0, 7));
        let focus_range = sel(pos(2, 0), pos(2, 3));
        let merged = merge_preview_text_ranges(&anchor_range, &focus_range);
        assert_eq!((merged.anchor, merged.focus), (pos(0, 3), pos(2, 3)));
    }

    /// 全选选区覆盖首行首列到末行末列；空文档返回 `None`。
    #[test]
    fn preview_select_all_covers_first_to_last_line() {
        let lines = vec!["ab".to_string(), "c".to_string()];
        let selection = preview_select_all_selection(&lines).unwrap();
        assert_eq!((selection.anchor, selection.focus), (pos(0, 0), pos(1, 1)));
        assert_eq!(
            selected_preview_text_from_lines(&lines, &selection).as_deref(),
            Some("ab\nc")
        );
        assert!(preview_select_all_selection(&[]).is_none());
    }

    /// 跨行提取按行拼接并以 `\n` 连接，首尾行只截取部分列。
    #[test]
    fn selected_preview_text_joins_lines_with_newline() {
        let lines = vec![
            "hello".to_string(),
            "world".to_string(),
            "again".to_string(),
        ];
        let selection = sel(pos(0, 3), pos(2, 2));
        assert_eq!(
            selected_preview_text_from_lines(&lines, &selection).as_deref(),
            Some("lo\nworld\nag")
        );
    }

    /// 向上拖拽产生的反向选区提取结果与正向一致。
    #[test]
    fn selected_preview_text_normalizes_reversed_selection() {
        let lines = vec!["hello".to_string(), "world".to_string()];
        let reversed = sel(pos(1, 5), pos(0, 3));
        assert_eq!(
            selected_preview_text_from_lines(&lines, &reversed).as_deref(),
            Some("lo\nworld")
        );
    }

    /// 空选区（锚点等于焦点）不提取任何文本，模拟释放时空选区被清除。
    #[test]
    fn empty_preview_selection_extracts_nothing() {
        let lines = vec!["ab".to_string()];
        let empty = sel(pos(0, 1), pos(0, 1));
        assert!(empty.is_empty());
        assert_eq!(selected_preview_text_from_lines(&lines, &empty), None);
    }

    /// 行内字节范围：首尾行按部分列截取（多字节字符按字符边界换算），中间行整行覆盖。
    #[test]
    fn preview_selection_byte_range_covers_partial_lines() {
        let lines = ["日bc".to_string(), "def".to_string(), "ghi".to_string()];
        let selection = sel(pos(0, 1), pos(2, 2));
        // "日bc"：第 1 列起 3 字节，到第 3 列共 5 字节。
        assert_eq!(
            preview_selection_byte_range_for_line(Some(&selection), 0, &lines[0]),
            Some(3..5)
        );
        assert_eq!(
            preview_selection_byte_range_for_line(Some(&selection), 1, &lines[1]),
            Some(0..3)
        );
        assert_eq!(
            preview_selection_byte_range_for_line(Some(&selection), 2, &lines[2]),
            Some(0..2)
        );
        assert_eq!(
            preview_selection_byte_range_for_line(Some(&selection), 3, "jk"),
            None
        );
        assert_eq!(
            preview_selection_byte_range_for_line(None, 0, &lines[0]),
            None
        );
    }
}
