//! 文件职责：渲染日志分析工作区的主内容区域。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-16
//! 作者：Argus 开发团队
//! 主要功能：按行虚拟渲染日志正文和 Jstack 分析页，大日志只读取当前可见页，避免整份日志进入 UI 文本节点。

use std::ops::Range;

use crate::app::{
    ArgusApp, LOG_VIEWER_TEXT_LEFT_PADDING, LOG_VIEWER_TEXT_RIGHT_PADDING, LogScrollbarAxis,
    LogScrollbarDrag, SEARCH_RESULT_PANEL_HEIGHT_MIN, SEARCH_RESULT_PANEL_RESERVED_HEIGHT,
    SearchResultListItem, SearchResultScrollbarAxis, SearchResultScrollbarDrag, SearchRunKind,
    TabKind, log_viewer_display_text, log_viewer_line_number_width,
};
use crate::fonts::ARGUS_LOG_FONT_FAMILY;
use crate::highlight::{
    HighlightCache, HighlightLanguage, HighlightSpan, HighlightTokenKind, detect_highlight_language,
};
use crate::infra::perf::PerfSpan;
use crate::infra::text_selection::{
    byte_index_for_character, char_column_for_byte_index, character_count, slice_character_range,
};
use crate::reader::log_file_reader::{LogDocument, LogOpenState, LogReaderHandle};
use crate::search::search_task::SearchTaskState;
use crate::theme::AppTheme;
use crate::ui::components::icon::ArgusIcon;
use crate::ui::components::icon::render_icon;
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::components::loading_spinner::render_loading_spinner;
use crate::ui::jstack_analysis_view;
use crate::ui::runtime_analysis_view;
use crate::ui::settings_page;
use crate::ui::sftp_file_manager_view;
use crate::ui::terminal_view;
use gpui::{
    AnyElement, Context, HighlightStyle, IntoElement, KeyDownEvent, ListHorizontalSizingBehavior,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ScrollWheelEvent, SharedString,
    StyledText, Window, canvas, div, point, prelude::*, px, rgb, uniform_list,
};

/// 日志正文固定行高；虚拟列表和分页窗口都依赖该值稳定换算。
const LOG_VIEWER_ROW_HEIGHT: f32 = 20.0;
/// 行号右侧打点标记尺寸；保持较小尺寸避免干扰行号读取。
const LOG_LINE_MARKER_SIZE: f32 = 5.0;
/// 行号打点距离行号列右侧的间距。
const LOG_LINE_MARKER_RIGHT: f32 = 5.0;
/// 首帧视口未测量时的默认渲染行数。
const DEFAULT_VISIBLE_ROWS: usize = 80;
/// 自绘滚动条宽度。
const LOG_SCROLLBAR_WIDTH: f32 = 5.0;
/// 自绘滚动条边距。
const LOG_SCROLLBAR_PADDING: f32 = 4.0;
/// 自绘滚动条最小滑块长度。
const LOG_SCROLLBAR_MIN_THUMB: f32 = 32.0;
/// 搜索结果面板固定行高。
const SEARCH_RESULT_ROW_HEIGHT: f32 = 28.0;
/// 搜索结果列表最小内容宽度，超出面板宽度时启用横向滚动条。
const SEARCH_RESULT_ROW_MIN_WIDTH: f32 = 760.0;
/// 搜索结果行左侧行号列宽度。
const SEARCH_RESULT_LINE_LABEL_WIDTH: f32 = 78.0;
/// 搜索结果行横向内边距总和。
const SEARCH_RESULT_ROW_HORIZONTAL_PADDING: f32 = 24.0;
/// 搜索结果行固定列间距。
const SEARCH_RESULT_ROW_GAP_WIDTH: f32 = 8.0;
/// 搜索结果中 ASCII 字符的宽度估算，用于提前撑开横向滚动内容。
const SEARCH_RESULT_ASCII_CHAR_WIDTH: f32 = 7.4;
/// 搜索结果中中文等宽字符的宽度估算，避免混排内容在面板中提前换行。
const SEARCH_RESULT_WIDE_CHAR_WIDTH: f32 = 13.0;
/// 搜索结果预览最大字符数；不截断结果数量，只限制单行预览渲染成本。
const SEARCH_RESULT_PREVIEW_MAX_CHARS: usize = 420;
/// 搜索结果预览中命中点前后的上下文字符数。
const SEARCH_RESULT_PREVIEW_CONTEXT_CHARS: usize = 160;
/// 分页日志横向切片的额外字符缓冲，避免轻微估算误差导致滚动边缘露白。
const PAGED_LOG_HORIZONTAL_OVERSCAN_COLUMNS: usize = 96;

mod log_lines;
mod scrollbars;
mod search_results;
mod text_helpers;

pub(crate) use log_lines::*;
pub(crate) use scrollbars::*;
pub(crate) use search_results::*;
pub(crate) use text_helpers::*;

/// 滚动条渲染和拖拽所需的度量数据。
#[derive(Clone, Copy, Debug)]
pub(crate) struct LogScrollbarMetrics {
    /// 滑块起点。
    thumb_start: gpui::Pixels,
    /// 滑块长度。
    thumb_length: gpui::Pixels,
    /// 轨道起点。
    track_start: gpui::Pixels,
    /// 轨道长度。
    track_length: gpui::Pixels,
    /// 最大滚动距离。
    max_scroll: gpui::Pixels,
}

/// 分页日志单行实际交给 GPUI 渲染的可见文本切片。
#[derive(Clone, Debug)]
pub(crate) struct LogVisibleText {
    /// 当前切片文本。
    text: String,
    /// 当前切片在完整展示文本中的字符范围。
    char_range: Range<usize>,
}

/// 渲染日志内容区。
///
/// 参数说明：
/// - `app`：应用状态，提供日志文档、主题和状态提示。
/// - `cx`：应用上下文，用于内容区工具按钮和日志选择事件。
///
/// 返回值：GPUI 元素树；真实来源会按读取状态展示加载、失败或逐行日志。
pub(crate) fn render(
    app: &mut ArgusApp,
    window: &mut Window,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement {
    let theme = app.theme.clone();

    div()
        .flex_1()
        .h_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(rgb(theme.content))
        .child(
            div()
                .flex_1()
                .min_h(px(0.0))
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(render_content_body(app, &theme, window, cx)),
        )
        .when(app.should_show_log_search_results(), |this| {
            this.child(render_search_results_panel(app, &theme, cx))
        })
}

/// 根据当前内容状态渲染主体区域。
fn search_progress_text(app: &ArgusApp) -> String {
    let progress = &app.log_search.progress;
    let scope = app.log_search.scope;
    let progress_part = match scope {
        crate::search::search_engine::SearchScope::CurrentFile => {
            format!("行进度 {}/{}", progress.scanned_lines, progress.total_lines)
        }
        crate::search::search_engine::SearchScope::Directory
        | crate::search::search_engine::SearchScope::SelectedFiles => {
            format!(
                "文件进度 {}/{}",
                progress.scanned_files, progress.total_files
            )
        }
    };
    let current = progress
        .current_path
        .as_ref()
        .map(|path| format!("，当前：{path}"))
        .unwrap_or_default();
    format!(
        "{}，{}，结果 {} 条{}",
        scope.label(),
        progress_part,
        app.log_search.results.len(),
        current
    )
}

/// 渲染内容区空状态或未读取提示。
fn render_empty_state(title: &str, detail: &str, app: &ArgusApp, theme: &AppTheme) -> AnyElement {
    render_empty_state_with_leading(title, detail, None, app, theme)
}

/// 渲染带加载图标的内容区提示。
fn render_loading_state(
    title: &str,
    detail: &str,
    source_id: crate::loader::SourceId,
    app: &ArgusApp,
    theme: &AppTheme,
) -> AnyElement {
    render_empty_state_with_leading(
        title,
        detail,
        Some(render_loading_spinner(
            ("log-reading-spinner", source_id.0),
            theme.foreground_muted,
            16.0,
        )),
        app,
        theme,
    )
}

/// 渲染内容区居中提示，可选在标题前追加一个状态图标。
fn render_empty_state_with_leading(
    title: &str,
    detail: &str,
    leading: Option<AnyElement>,
    app: &ArgusApp,
    theme: &AppTheme,
) -> AnyElement {
    let detail_font_size = app.log_content_font_size;
    let title_font_size = detail_font_size + 4.0;
    let title_row = div()
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .text_size(px(title_font_size))
        .text_color(rgb(theme.foreground))
        .children(leading)
        .child(title.to_string());

    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .overflow_hidden()
        .bg(rgb(theme.content))
        .child(
            div()
                .w(px(520.0))
                .max_w_full()
                .px_6()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .text_center()
                .child(title_row)
                .child(
                    div()
                        .text_size(px(detail_font_size))
                        .text_color(rgb(theme.foreground_muted))
                        .child(detail.to_string()),
                ),
        )
        .into_any_element()
}

/// 计算分页视口当前应该渲染的行数。
fn visible_row_capacity(viewport_height: gpui::Pixels) -> usize {
    if viewport_height <= px(0.0) {
        return DEFAULT_VISIBLE_ROWS;
    }

    ((f32::from(viewport_height) / LOG_VIEWER_ROW_HEIGHT).ceil() as usize + 2)
        .max(1)
        .min(400)
}

/// 计算分页日志最大纵向滚动像素。
fn paged_vertical_max_scroll(line_count: usize, viewport_height: gpui::Pixels) -> f64 {
    let content_height = line_count as f64 * LOG_VIEWER_ROW_HEIGHT as f64;
    let viewport_height = f64::from(viewport_height).max(0.0);
    (content_height - viewport_height).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证选区覆盖语法高亮中段时，只覆盖被选中的范围，两侧语法颜色仍会保留。
    #[test]
    fn selection_splits_overlapping_syntax_highlight() {
        let theme = AppTheme::dark();
        let highlights = merge_syntax_and_selection_highlights(
            vec![HighlightSpan {
                range: 0..15,
                kind: HighlightTokenKind::StackClass,
            }],
            Some(5..9),
            &theme,
        );
        let ranges = highlights
            .iter()
            .map(|(range, _)| range.clone())
            .collect::<Vec<_>>();

        assert_eq!(ranges, vec![0..5, 5..9, 9..15]);
    }

    /// 验证搜索跳转行背景不再复用文本选区色，避免选中当前行时视觉混淆。
    #[test]
    fn active_search_line_background_differs_from_selection() {
        let theme = AppTheme::dark();
        let background = active_search_line_background(&theme);

        assert_ne!(background, theme.selection);
        assert_ne!(background, theme.content);
        assert_eq!(blend_rgb(0x000000, 0xffffff, 0.5), 0x808080);
    }

    /// 验证分页长行可按展示列直接截取，并保持 tab 展开为 4 个空格。
    #[test]
    fn paged_visible_text_slices_raw_line_without_full_expansion() {
        let visible = visible_log_text_from_raw("ab\tcdef", Some(&(2..8)));

        assert_eq!(visible.text, "    cd");
        assert_eq!(visible.char_range, 2..8);
    }
}
