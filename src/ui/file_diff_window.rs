//! 文件职责：用独立窗口并排展示两个文件版本的完整内容与差异。
//! 创建日期：2026-10-29
//! 作者：Argus 开发团队
//! 主要功能：按行对齐两侧完整文件，使用虚拟列表平滑滚动，并展示各自行号、增删高亮和悬停滚动条。

use std::sync::Arc;
use std::time::Duration;

#[cfg(not(target_os = "windows"))]
use gpui::WindowControlArea;
use gpui::{
    Context, Entity, FontWeight, IntoElement, ListAlignment, ListState, MouseButton,
    MouseDownEvent, Pixels, Render, Rgba, Subscription, Window, div, list, prelude::*, px, rgb,
    rgba,
};
use similar::{DiffTag, TextDiff};

use crate::app::ArgusApp;
use crate::fonts::ARGUS_LOG_FONT_FAMILY;
use crate::platform::custom_titlebar;
use crate::remote::remote_file::FilePreviewContent;
use crate::theme::AppTheme;
use crate::ui::components::scrollbar::{render_list_vertical_scrollbar, scroll_area_hover_handler};
use crate::ui::custom_title_bar::{TITLE_BAR_HEIGHT, platform_window_controls};

/// diff 窗口默认宽度；并排双栏需要比统一 diff 更宽的阅读区域。
pub(crate) const FILE_DIFF_WINDOW_WIDTH: f32 = 1280.0;
/// diff 窗口默认高度。
pub(crate) const FILE_DIFF_WINDOW_HEIGHT: f32 = 760.0;
/// diff 窗口最小宽度。
pub(crate) const FILE_DIFF_WINDOW_MIN_WIDTH: f32 = 900.0;
/// diff 窗口最小高度。
pub(crate) const FILE_DIFF_WINDOW_MIN_HEIGHT: f32 = 480.0;
/// 文件内容字号。
const FILE_DIFF_FONT_SIZE: f32 = 12.0;
/// 单行文本行高；换行后的视觉行继续沿用该行高。
const FILE_DIFF_LINE_HEIGHT: f32 = 18.0;
/// 行内容上下内边距。
const FILE_DIFF_ROW_VERTICAL_PADDING: f32 = 1.0;
/// 单行最小高度，保证空行仍有稳定的行号对齐。
const FILE_DIFF_ROW_MIN_HEIGHT: f32 = FILE_DIFF_LINE_HEIGHT + FILE_DIFF_ROW_VERTICAL_PADDING * 2.0;
/// 避免异常超大 diff 一次性创建过多界面行。
const FILE_DIFF_MAX_ROWS: usize = 20_000;
/// 复杂 diff 的计算上限；超时后由 similar 返回近似结果，避免卡住界面。
const FILE_DIFF_TIMEOUT: Duration = Duration::from_millis(500);
/// 行号数字宽度估算。
const FILE_DIFF_LINE_NUMBER_CHAR_WIDTH: f32 = 7.2;
/// 行号栏最小宽度。
const FILE_DIFF_LINE_NUMBER_MIN_WIDTH: f32 = 34.0;
/// 增删标记栏宽度。
const FILE_DIFF_MARKER_WIDTH: f32 = 14.0;
/// 行内容左右内边距。
const FILE_DIFF_CELL_HORIZONTAL_PADDING: f32 = 6.0;
/// 左右版本标题栏高度。
const FILE_DIFF_COLUMN_HEADER_HEIGHT: f32 = 30.0;
/// 增删行背景透明度。
const FILE_DIFF_CHANGED_BACKGROUND_ALPHA: u32 = 0x24;

/// 一条参与并排展示的文件行。
#[derive(Clone, Debug)]
struct DiffLine {
    /// 该行在所属版本中的 1 基行号。
    number: usize,
    /// 行文本，不含换行符。
    text: String,
}

/// 两侧按行对齐后的一行。
#[derive(Clone, Debug)]
struct DiffRow {
    /// 当前版本行；为空表示该行只存在于对比版本。
    left: Option<DiffLine>,
    /// 对比版本行；为空表示该行只在当前版本中被删除。
    right: Option<DiffLine>,
    /// 当前版本行是否参与删除或修改高亮。
    left_changed: bool,
    /// 对比版本行是否参与新增或修改高亮。
    right_changed: bool,
}

/// 构建完成的并排 diff 内容。
#[derive(Clone, Debug)]
struct DiffContent {
    rows: Vec<DiffRow>,
    notices: Vec<String>,
    left_line_count: usize,
    right_line_count: usize,
}

/// 并排 diff 的左右侧。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiffSide {
    Left,
    Right,
}

impl DiffSide {
    /// 返回该侧增删高亮使用的主题颜色。
    fn accent(self, theme: &AppTheme) -> u32 {
        match self {
            Self::Left => theme.error,
            Self::Right => theme.success,
        }
    }

    /// 返回该侧变化行使用的增删标记。
    fn marker(self) -> &'static str {
        match self {
            Self::Left => "-",
            Self::Right => "+",
        }
    }
}

/// 独立文件 diff 窗口。
pub(crate) struct FileDiffWindow {
    theme: AppTheme,
    title: String,
    left_label: String,
    right_label: String,
    rows: Arc<Vec<DiffRow>>,
    notices: Arc<Vec<String>>,
    line_number_width: Pixels,
    list_state: ListState,
    _app_observer: Subscription,
}

impl FileDiffWindow {
    /// 创建并排文件 diff 窗口。
    pub(crate) fn new(
        app: Entity<ArgusApp>,
        theme: AppTheme,
        file_name: String,
        current_version: String,
        target_version: String,
        current_content: FilePreviewContent,
        target_content: FilePreviewContent,
        cx: &mut Context<Self>,
    ) -> Self {
        let content = build_diff_content(file_name.as_str(), current_content, target_content);
        let line_number_width =
            diff_line_number_width(content.left_line_count.max(content.right_line_count));
        let left_label = diff_column_label("当前版本", &current_version, content.left_line_count);
        let right_label = diff_column_label("对比版本", &target_version, content.right_line_count);
        let row_count = content.rows.len();
        let _app_observer = cx.observe(&app, |view, app, cx| {
            let theme = app.read_with(cx, |app, _| app.theme.clone());
            if view.theme == theme {
                return;
            }
            view.theme = theme;
            cx.notify();
        });
        Self {
            theme,
            title: format!("{file_name} · 版本对比"),
            left_label,
            right_label,
            rows: Arc::new(content.rows),
            notices: Arc::new(content.notices),
            line_number_width,
            list_state: ListState::new(row_count, ListAlignment::Top, px(400.0)),
            _app_observer,
        }
    }
}

impl Render for FileDiffWindow {
    /// 渲染标题栏、两侧版本标题、提示和统一滚动的完整文件对比正文。
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let rows = self.rows.clone();
        let notices = self.notices.clone();
        let line_number_width = self.line_number_width;
        let notice_text = (!notices.is_empty()).then(|| notices.join("；"));

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme.content))
            .font_family(ARGUS_LOG_FONT_FAMILY)
            .text_color(rgb(theme.foreground))
            .child(
                // 透明原生标题栏下必须自行保留交通灯安全区，否则标题会被窗口按钮遮挡。
                div()
                    .h(px(TITLE_BAR_HEIGHT))
                    .w_full()
                    .flex_none()
                    .flex()
                    .items_center()
                    .bg(rgb(theme.title_bar))
                    .occlude()
                    .child(platform_window_controls(window.is_maximized(), &theme))
                    .child(
                        div()
                            .pl(px(8.0))
                            .flex_none()
                            .text_size(px(13.0))
                            .line_height(px(18.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(theme.foreground))
                            .child(self.title.clone()),
                    )
                    .child(diff_title_drag_area()),
            )
            .child(
                div()
                    .h(px(FILE_DIFF_COLUMN_HEADER_HEIGHT))
                    .w_full()
                    .flex_none()
                    .flex()
                    .items_center()
                    .bg(rgb(theme.title_bar))
                    .border_b_1()
                    .border_color(rgb(theme.border))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .px_3()
                            .text_size(px(11.0))
                            .text_color(rgb(theme.foreground_muted))
                            .child(self.left_label.clone()),
                    )
                    .child(div().w(px(1.0)).h_full().flex_none().bg(rgb(theme.border)))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .px_3()
                            .text_size(px(11.0))
                            .text_color(rgb(theme.foreground_muted))
                            .child(self.right_label.clone()),
                    ),
            )
            .when_some(notice_text, |this, notice| {
                this.child(
                    div()
                        .flex_none()
                        .w_full()
                        .px_4()
                        .py_2()
                        .bg(rgb(theme.current_line))
                        .border_b_1()
                        .border_color(rgb(theme.border))
                        .text_size(px(11.0))
                        .line_height(px(16.0))
                        .text_color(rgb(theme.foreground_muted))
                        .child(notice),
                )
            })
            .child(
                div()
                    .id("file-diff-scroll")
                    .debug_selector(|| "file-diff-scroll".to_string())
                    .relative()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_hidden()
                    .on_hover(scroll_area_hover_handler("file-diff-scroll".to_string()))
                    .child(
                        list(self.list_state.clone(), {
                            let rows = rows.clone();
                            let theme = theme.clone();
                            move |index, _, _| {
                                render_diff_row(&rows[index], &theme, line_number_width)
                                    .into_any_element()
                            }
                        })
                        .size_full(),
                    )
                    .children(render_list_vertical_scrollbar(
                        self.list_state.clone(),
                        "file-diff-scroll",
                        "file-diff-scrollbar",
                        &theme,
                    )),
            )
    }
}

/// 渲染标题栏可拖拽空白区域，并在双击时执行系统级最大化/还原。
fn diff_title_drag_area() -> impl IntoElement {
    let drag_area = div().id("file-diff-title-drag-area").h_full().flex_1();
    // Windows 使用普通客户区事件调用显式窗口操作，其余平台保留 GPUI 控制区语义。
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

/// 渲染左右对齐的一行；单侧缺失时保留占位背景。
fn render_diff_row(row: &DiffRow, theme: &AppTheme, line_number_width: Pixels) -> impl IntoElement {
    div()
        .w_full()
        .min_h(px(FILE_DIFF_ROW_MIN_HEIGHT))
        .flex()
        .child(render_diff_cell(
            DiffSide::Left,
            row.left.as_ref(),
            row.left_changed,
            theme,
            line_number_width,
        ))
        .child(div().w(px(1.0)).flex_none().bg(rgb(theme.border)))
        .child(render_diff_cell(
            DiffSide::Right,
            row.right.as_ref(),
            row.right_changed,
            theme,
            line_number_width,
        ))
}

/// 渲染某一侧的行号、增删标记和可换行文本。
fn render_diff_cell(
    side: DiffSide,
    line: Option<&DiffLine>,
    is_changed: bool,
    theme: &AppTheme,
    line_number_width: Pixels,
) -> impl IntoElement {
    let has_line = line.is_some();
    let background = if !has_line {
        rgb(theme.background)
    } else if is_changed {
        tint(side.accent(theme), FILE_DIFF_CHANGED_BACKGROUND_ALPHA)
    } else {
        rgb(theme.content)
    };
    let change_color = if is_changed {
        side.accent(theme)
    } else {
        theme.foreground_muted
    };
    let marker = if is_changed { side.marker() } else { "" };
    let number = line.map(|line| line.number.to_string()).unwrap_or_default();
    let text = line.map(|line| line.text.clone()).unwrap_or_default();

    div()
        .flex_1()
        .min_w(px(0.0))
        .flex()
        .items_start()
        .bg(background)
        .child(
            div()
                .w(px(FILE_DIFF_MARKER_WIDTH))
                .flex_none()
                .py(px(FILE_DIFF_ROW_VERTICAL_PADDING))
                .text_size(px(FILE_DIFF_FONT_SIZE))
                .line_height(px(FILE_DIFF_LINE_HEIGHT))
                .text_center()
                .text_color(rgb(change_color))
                .child(marker),
        )
        .child(
            div()
                .w(line_number_width)
                .flex_none()
                .py(px(FILE_DIFF_ROW_VERTICAL_PADDING))
                .pr_1()
                .text_size(px(FILE_DIFF_FONT_SIZE))
                .line_height(px(FILE_DIFF_LINE_HEIGHT))
                .text_right()
                .text_color(rgb(change_color))
                .child(number),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .px(px(FILE_DIFF_CELL_HORIZONTAL_PADDING))
                .py(px(FILE_DIFF_ROW_VERTICAL_PADDING))
                .text_size(px(FILE_DIFF_FONT_SIZE))
                .line_height(px(FILE_DIFF_LINE_HEIGHT))
                .text_color(rgb(theme.foreground))
                .child(text),
        )
}

/// 按行对齐两个完整版本；行号分别取各自版本中的真实位置。
fn build_diff_content(
    file_name: &str,
    current_content: FilePreviewContent,
    target_content: FilePreviewContent,
) -> DiffContent {
    let (current, current_truncated) = match current_content {
        FilePreviewContent::Text { content, truncated } => (content, truncated),
        FilePreviewContent::Binary => {
            return diff_notice_content(format!("{file_name}：当前版本是二进制文件，无法对比"));
        }
        FilePreviewContent::Error(error) => {
            return diff_notice_content(format!("{file_name}：当前版本读取失败：{error}"));
        }
    };
    let (target, target_truncated) = match target_content {
        FilePreviewContent::Text { content, truncated } => (content, truncated),
        FilePreviewContent::Binary => {
            return diff_notice_content(format!("{file_name}：对比版本是二进制文件，无法对比"));
        }
        FilePreviewContent::Error(error) => {
            return diff_notice_content(format!("{file_name}：对比版本读取失败：{error}"));
        }
    };

    let left_lines = current.lines().collect::<Vec<_>>();
    let right_lines = target.lines().collect::<Vec<_>>();
    let left_line_count = left_lines.len();
    let right_line_count = right_lines.len();
    let mut notices = Vec::new();
    if current_truncated || target_truncated {
        notices.push("文件超过 2 MiB，已截断后对比".to_string());
    }

    if current == target {
        if left_lines.is_empty() {
            notices.push("两个版本均为空文件".to_string());
        } else if current_truncated || target_truncated {
            notices.push("已读取部分内容相同".to_string());
        } else {
            notices.push("两个版本内容相同".to_string());
        }
        let rows = left_lines
            .iter()
            .take(FILE_DIFF_MAX_ROWS)
            .enumerate()
            .map(|(index, line)| DiffRow {
                left: Some(DiffLine {
                    number: index + 1,
                    text: (*line).to_string(),
                }),
                right: Some(DiffLine {
                    number: index + 1,
                    text: (*line).to_string(),
                }),
                left_changed: false,
                right_changed: false,
            })
            .collect();
        if left_lines.len() > FILE_DIFF_MAX_ROWS {
            notices.push(diff_row_limit_notice());
        }
        return DiffContent {
            rows,
            notices,
            left_line_count,
            right_line_count,
        };
    }

    // 直接按已拆分好的行比较，确保 diff 区间与展示行索引完全一致，
    // 同时避免仅因文件末尾换行符不同而出现两侧文本相同的“修改行”。
    let diff = TextDiff::configure()
        .timeout(FILE_DIFF_TIMEOUT)
        .diff_slices(&left_lines, &right_lines);
    let mut rows = Vec::new();
    let mut is_row_limit_reached = false;
    'ops: for op in diff.ops() {
        let old_range = op.old_range();
        let new_range = op.new_range();
        match op.tag() {
            DiffTag::Equal => {
                for offset in 0..old_range.len() {
                    rows.push(DiffRow {
                        left: Some(diff_line(&left_lines, old_range.start + offset)),
                        right: Some(diff_line(&right_lines, new_range.start + offset)),
                        left_changed: false,
                        right_changed: false,
                    });
                    if rows.len() >= FILE_DIFF_MAX_ROWS {
                        is_row_limit_reached = true;
                        break 'ops;
                    }
                }
            }
            DiffTag::Delete => {
                for offset in 0..old_range.len() {
                    rows.push(DiffRow {
                        left: Some(diff_line(&left_lines, old_range.start + offset)),
                        right: None,
                        left_changed: true,
                        right_changed: false,
                    });
                    if rows.len() >= FILE_DIFF_MAX_ROWS {
                        is_row_limit_reached = true;
                        break 'ops;
                    }
                }
            }
            DiffTag::Insert => {
                for offset in 0..new_range.len() {
                    rows.push(DiffRow {
                        left: None,
                        right: Some(diff_line(&right_lines, new_range.start + offset)),
                        left_changed: false,
                        right_changed: true,
                    });
                    if rows.len() >= FILE_DIFF_MAX_ROWS {
                        is_row_limit_reached = true;
                        break 'ops;
                    }
                }
            }
            DiffTag::Replace => {
                // 修改块按行号位置一一对齐；两侧行数不等时，多出的行在另一侧留空。
                for offset in 0..old_range.len().max(new_range.len()) {
                    let left = (offset < old_range.len())
                        .then(|| diff_line(&left_lines, old_range.start + offset));
                    let right = (offset < new_range.len())
                        .then(|| diff_line(&right_lines, new_range.start + offset));
                    let left_changed = left.is_some();
                    let right_changed = right.is_some();
                    rows.push(DiffRow {
                        left,
                        right,
                        left_changed,
                        right_changed,
                    });
                    if rows.len() >= FILE_DIFF_MAX_ROWS {
                        is_row_limit_reached = true;
                        break 'ops;
                    }
                }
            }
        }
    }
    if is_row_limit_reached {
        notices.push(diff_row_limit_notice());
    }
    if rows.is_empty() {
        notices.push("两个版本均无内容".to_string());
    } else if !is_row_limit_reached && !rows.iter().any(|row| row.left_changed || row.right_changed)
    {
        notices.push(if current_truncated || target_truncated {
            "已读取部分内容相同".to_string()
        } else {
            "两个版本内容相同".to_string()
        });
    }

    DiffContent {
        rows,
        notices,
        left_line_count,
        right_line_count,
    }
}

/// 构造展示行数超限提示。
fn diff_row_limit_notice() -> String {
    format!("对比行数超过 {FILE_DIFF_MAX_ROWS}，仅显示前 {FILE_DIFF_MAX_ROWS} 行")
}

/// 构造无法展示正文时的提示内容。
fn diff_notice_content(notice: String) -> DiffContent {
    DiffContent {
        rows: Vec::new(),
        notices: vec![notice],
        left_line_count: 0,
        right_line_count: 0,
    }
}

/// 从已拆分行中构造带 1 基行号的展示行；索引越界说明 diff 区间与文本不一致。
fn diff_line(lines: &[&str], index: usize) -> DiffLine {
    DiffLine {
        number: index + 1,
        text: (*lines.get(index).expect("diff 行号超出文件行数")).to_string(),
    }
}

/// 根据最大行号计算行号栏宽度。
fn diff_line_number_width(max_line_number: usize) -> Pixels {
    let digits = max_line_number.max(1).to_string().len();
    px((digits as f32 * FILE_DIFF_LINE_NUMBER_CHAR_WIDTH + 18.0)
        .max(FILE_DIFF_LINE_NUMBER_MIN_WIDTH))
}

/// 构造两侧版本标题；空内容不展示行数。
fn diff_column_label(prefix: &str, version: &str, line_count: usize) -> String {
    if line_count == 0 {
        format!("{prefix} · {version}")
    } else {
        format!("{prefix} · {version} · {line_count} 行")
    }
}

/// 给主题颜色叠加强度较低的透明背景。
fn tint(color: u32, alpha: u32) -> Rgba {
    rgba(((color & 0x00ff_ffff) << 8) | (alpha & 0xff))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造未截断的文本预览内容。
    fn text(content: &str) -> FilePreviewContent {
        FilePreviewContent::Text {
            content: content.to_string(),
            truncated: false,
        }
    }

    /// 返回展示行的行号和文本。
    fn line_info(line: &Option<DiffLine>) -> Option<(usize, &str)> {
        line.as_ref().map(|line| (line.number, line.text.as_str()))
    }

    /// 验证修改行在两侧按相同行号并排展示并同时高亮。
    #[test]
    fn replaced_lines_are_paired_with_both_line_numbers() {
        let content = build_diff_content("app.rs", text("alpha\nbeta\n"), text("alpha\ngamma\n"));

        assert!(content.notices.is_empty());
        assert_eq!(content.rows.len(), 2);
        assert_eq!(line_info(&content.rows[0].left), Some((1, "alpha")));
        assert_eq!(line_info(&content.rows[0].right), Some((1, "alpha")));
        assert!(!content.rows[0].left_changed);
        assert!(!content.rows[0].right_changed);

        assert_eq!(line_info(&content.rows[1].left), Some((2, "beta")));
        assert_eq!(line_info(&content.rows[1].right), Some((2, "gamma")));
        assert!(content.rows[1].left_changed);
        assert!(content.rows[1].right_changed);
    }

    /// 验证删除行只出现在当前版本一侧，对比侧保留空位。
    #[test]
    fn deleted_line_leaves_right_side_blank() {
        let content = build_diff_content("app.rs", text("a\nb\nc\n"), text("a\nc\n"));

        assert_eq!(content.rows.len(), 3);
        assert_eq!(line_info(&content.rows[1].left), Some((2, "b")));
        assert_eq!(line_info(&content.rows[1].right), None);
        assert!(content.rows[1].left_changed);
        assert!(!content.rows[1].right_changed);
        assert_eq!(line_info(&content.rows[2].left), Some((3, "c")));
        assert_eq!(line_info(&content.rows[2].right), Some((2, "c")));
    }

    /// 验证新增行只出现在对比版本一侧，当前版本侧保留空位。
    #[test]
    fn inserted_line_leaves_left_side_blank() {
        let content = build_diff_content("app.rs", text("a\nc\n"), text("a\nb\nc\n"));

        assert_eq!(content.rows.len(), 3);
        assert_eq!(line_info(&content.rows[1].left), None);
        assert_eq!(line_info(&content.rows[1].right), Some((2, "b")));
        assert!(!content.rows[1].left_changed);
        assert!(content.rows[1].right_changed);
        assert_eq!(line_info(&content.rows[2].left), Some((2, "c")));
        assert_eq!(line_info(&content.rows[2].right), Some((3, "c")));
    }

    /// 验证内容相同时仍展示完整文件，而不是只显示一条提示。
    #[test]
    fn identical_content_still_shows_all_lines() {
        let content = build_diff_content("app.rs", text("a\nb\nc\n"), text("a\nb\nc\n"));

        assert_eq!(content.rows.len(), 3);
        assert!(
            content
                .notices
                .iter()
                .any(|notice| notice.contains("内容相同"))
        );
        assert_eq!(line_info(&content.rows[2].left), Some((3, "c")));
        assert_eq!(line_info(&content.rows[2].right), Some((3, "c")));
    }

    /// 验证仅文件末尾换行符不同时仍视为内容相同，不误标修改行。
    #[test]
    fn trailing_newline_only_difference_is_not_highlighted() {
        let content = build_diff_content("app.rs", text("a\nb\n"), text("a\nb"));

        assert_eq!(content.rows.len(), 2);
        assert!(
            content
                .rows
                .iter()
                .all(|row| !row.left_changed && !row.right_changed)
        );
        assert!(
            content
                .notices
                .iter()
                .any(|notice| notice.contains("内容相同"))
        );
    }

    /// 验证空文件、二进制和读取失败都能给出明确提示。
    #[test]
    fn empty_binary_and_error_content_report_notice() {
        let content = build_diff_content("empty.txt", text(""), text(""));
        assert!(content.rows.is_empty());
        assert!(
            content
                .notices
                .iter()
                .any(|notice| notice.contains("均为空文件"))
        );

        let content = build_diff_content("a.bin", FilePreviewContent::Binary, text("x"));
        assert!(content.rows.is_empty());
        assert!(
            content
                .notices
                .iter()
                .any(|notice| notice.contains("当前版本是二进制文件"))
        );

        let content = build_diff_content(
            "a.rs",
            text("x"),
            FilePreviewContent::Error("超时".to_string()),
        );
        assert!(content.rows.is_empty());
        assert!(
            content
                .notices
                .iter()
                .any(|notice| notice.contains("对比版本读取失败：超时"))
        );
    }

    /// 验证截断内容仍展示已读取行，并带截断提示。
    #[test]
    fn truncated_content_keeps_rows_and_adds_notice() {
        let content = build_diff_content(
            "app.rs",
            FilePreviewContent::Text {
                content: "a\n".to_string(),
                truncated: true,
            },
            FilePreviewContent::Text {
                content: "b\n".to_string(),
                truncated: true,
            },
        );

        assert!(
            content
                .notices
                .iter()
                .any(|notice| notice.contains("2 MiB"))
        );
        assert_eq!(content.rows.len(), 1);
        assert_eq!(line_info(&content.rows[0].left), Some((1, "a")));
        assert_eq!(line_info(&content.rows[0].right), Some((1, "b")));
    }

    /// 验证相同内容的超大文件也受展示行数上限保护。
    #[test]
    fn identical_content_respects_row_limit() {
        let large = "x\n".repeat(FILE_DIFF_MAX_ROWS + 1);
        let content = build_diff_content("big.txt", text(&large), text(&large));

        assert_eq!(content.rows.len(), FILE_DIFF_MAX_ROWS);
        assert!(
            content
                .notices
                .iter()
                .any(|notice| notice.contains("对比行数超过"))
        );
    }

    /// 验证行号栏随最大行号位数增长。
    #[test]
    fn line_number_width_grows_with_digits() {
        assert!(diff_line_number_width(9) < diff_line_number_width(100));
        assert!(diff_line_number_width(100) < diff_line_number_width(10_000));
        assert_eq!(
            diff_line_number_width(0),
            px(FILE_DIFF_LINE_NUMBER_MIN_WIDTH)
        );
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use crate::config::ConfigManager;
    use crate::config::paths::isolated_test_dir;
    use gpui::{Modifiers, MouseButton, TestAppContext, point, size};

    /// 验证并排窗口能完成布局并创建统一滚动区域。
    #[gpui::test]
    fn side_by_side_window_renders_unified_scroll_area(cx: &mut TestAppContext) {
        let app = cx.new(|_| {
            ArgusApp::new_with_config_manager(ConfigManager::new(
                isolated_test_dir("file-diff-window").join("settings.toml"),
            ))
        });
        let theme = cx.read(|cx| app.read(cx).theme.clone());
        let (_window, cx) = cx.add_window_view(move |_, cx| {
            FileDiffWindow::new(
                app,
                theme,
                "app.rs".to_string(),
                "HEAD (r42)".to_string(),
                "r7".to_string(),
                FilePreviewContent::Text {
                    content: "alpha\nbeta\n".to_string(),
                    truncated: false,
                },
                FilePreviewContent::Text {
                    content: "alpha\ngamma\n".to_string(),
                    truncated: false,
                },
                cx,
            )
        });
        cx.simulate_resize(size(px(1280.0), px(760.0)));

        let bounds = cx
            .debug_bounds("file-diff-scroll")
            .expect("应创建并排 diff 的统一滚动区域");
        assert!(bounds.size.height > px(0.0));
        assert!(bounds.size.width > px(0.0));
    }

    /// 验证虚拟列表滚动条悬停显示，并可拖拽改变滚动位置。
    #[gpui::test]
    fn scrollbar_shows_on_hover_and_drags(cx: &mut TestAppContext) {
        let app = cx.new(|_| {
            ArgusApp::new_with_config_manager(ConfigManager::new(
                isolated_test_dir("file-diff-scrollbar").join("settings.toml"),
            ))
        });
        let theme = cx.read(|cx| app.read(cx).theme.clone());
        let content = (0..240)
            .map(|index| format!("line {index:03}"))
            .collect::<Vec<_>>()
            .join("\n");
        let changed = content.replace("line 120", "line changed");
        let (view, cx) = cx.add_window_view(move |_, cx| {
            FileDiffWindow::new(
                app,
                theme,
                "app.rs".to_string(),
                "HEAD".to_string(),
                "r7".to_string(),
                FilePreviewContent::Text {
                    content,
                    truncated: false,
                },
                FilePreviewContent::Text {
                    content: changed,
                    truncated: false,
                },
                cx,
            )
        });
        cx.simulate_resize(size(px(1280.0), px(760.0)));
        let scroll_area = cx
            .debug_bounds("file-diff-scroll")
            .expect("应存在 diff 滚动区域");
        cx.simulate_mouse_move(scroll_area.center(), None, Modifiers::default());
        toggle_window_width(cx);
        assert!(crate::ui::components::scrollbar::scrollbar_hovered(
            "file-diff-scroll"
        ));
        let thumb = cx
            .debug_bounds("file-diff-scrollbar")
            .expect("悬停时应显示纵向滚动条");

        let before = view.read_with(cx, |view, _| {
            view.list_state.scroll_px_offset_for_scrollbar()
        });
        cx.simulate_mouse_down(thumb.center(), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(
            point(thumb.center().x, scroll_area.bottom() - px(8.0)),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        cx.simulate_mouse_up(
            point(thumb.center().x, scroll_area.bottom() - px(8.0)),
            MouseButton::Left,
            Modifiers::default(),
        );
        let after = view.read_with(cx, |view, _| {
            view.list_state.scroll_px_offset_for_scrollbar()
        });
        assert_ne!(before, after, "拖拽滚动条应改变列表滚动位置");

        cx.simulate_mouse_move(
            point(scroll_area.center().x, px(10.0)),
            None,
            Modifiers::default(),
        );
        toggle_window_width(cx);
        assert!(!crate::ui::components::scrollbar::scrollbar_hovered(
            "file-diff-scroll"
        ));
    }

    /// 通过一像素窗口尺寸变化强制完成一次布局和绘制。
    fn toggle_window_width(cx: &mut gpui::VisualTestContext) {
        let viewport = cx.update(|window, _| window.viewport_size());
        let width = if viewport.width == px(1280.0) {
            1281.0
        } else {
            1280.0
        };
        cx.simulate_resize(size(px(width), viewport.height));
    }
}
