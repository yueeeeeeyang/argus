//! 文件职责：渲染带语法高亮的远程文件只读预览独立窗口。
//! 创建日期：2026-07-03
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：以项目统一编辑器样式展示远程文本、代码高亮、行号、截断状态及读取失败提示。

use std::ops::Range;

use gpui::{
    AnyElement, Context, Entity, FocusHandle, FontWeight, HighlightStyle, IntoElement, Render,
    StyledText, Subscription, UniformListScrollHandle, Window, div, prelude::*, px, rgb,
    uniform_list,
};

use crate::app::{ArgusApp, log_viewer_line_number_width, observe_app_theme};
use crate::fonts::{ARGUS_LOG_FONT_FAMILY, ARGUS_UI_FONT_FAMILY};
use crate::highlight::{
    HighlightCache, HighlightLanguage, HighlightSpan, detect_highlight_language,
};
use crate::remote::remote_file::FilePreviewContent;
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::window_title_bar::render_window_title_bar;
use crate::ui::custom_title_bar::TITLE_BAR_HEIGHT;
use crate::ui::highlight_colors::{HighlightColorContext, color_for_highlight_token};

/// 预览窗口标题图标尺寸。
const FILE_PREVIEW_TITLE_ICON_SIZE: f32 = 16.0;
/// 预览正文行高，保持与日志阅读区一致的高密度展示。
const FILE_PREVIEW_ROW_HEIGHT: f32 = 20.0;
/// 预览正文字号。
const FILE_PREVIEW_FONT_SIZE: f32 = 12.0;
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
            _app_observer,
        }
    }

    /// 渲染预览窗口标题栏。
    fn render_header(&self) -> impl IntoElement {
        let theme = self.theme.clone();
        let file_name = self.file_name.clone();

        // 左上角只保留文件图标和文件名；语言类型统一放在正文上下文栏，避免挤占关闭按钮前空间。
        let left = div()
            .flex()
            .items_center()
            .gap_2()
            .flex_1()
            .min_w(px(0.0))
            .child(render_icon(
                ArgusIcon::FileText,
                theme.foreground_muted,
                FILE_PREVIEW_TITLE_ICON_SIZE,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .text_size(px(13.0))
                    .line_height(px(18.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(theme.foreground))
                    .truncate()
                    .child(file_name),
            );

        // 标题栏骨架（高度、分隔线、关闭按钮）由共享组件统一，避免多窗口漂移。
        render_window_title_bar(
            "file-preview-window-close",
            "关闭预览",
            TITLE_BAR_HEIGHT,
            true,
            &self.theme,
            left,
            move |_, window, _| {
                window.remove_window();
            },
        )
    }

    /// 渲染底部只读状态栏，保持与主窗口状态栏的背景和信息密度一致。
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
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(rgb(self.theme.border))
            .bg(rgb(self.theme.status_bar))
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
                    // 行号栏底色独立于虚拟列表行存在，短文件和列表底部也能连续填满整个正文高度。
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left_0()
                            .w(px(line_number_width))
                            .border_r_1()
                            .border_color(rgb(self.theme.border))
                            .bg(rgb(self.theme.side_bar)),
                    )
                    .child(
                        uniform_list(
                            "file-preview-lines",
                            line_count,
                            cx.processor(move |this, range: Range<usize>, _window, _cx| {
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
                                        let syntax_spans = this.highlight_cache.highlight_line(
                                            line_number - 1,
                                            this.language,
                                            line,
                                        );
                                        render_preview_line(
                                            line_number,
                                            line,
                                            line_number_width,
                                            syntax_spans,
                                            &this.theme,
                                        )
                                        .into_any_element()
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .size_full()
                        .track_scroll(self.scroll.clone()),
                    )
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
    /// 渲染预览窗口主体。
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root_focus_for_click = self.root_focus.clone();
        div()
            .id("file-preview-window-root")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(self.theme.content))
            .font_family(ARGUS_UI_FONT_FAMILY)
            .text_color(rgb(self.theme.foreground))
            .occlude()
            .focusable()
            .track_focus(&self.root_focus)
            .on_click(move |_, window, _| {
                root_focus_for_click.focus(window);
            })
            .child(self.render_header())
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .font_family(ARGUS_LOG_FONT_FAMILY)
                    .child(self.render_body(cx)),
            )
            .child(self.render_status_bar())
    }
}

/// 渲染预览正文单行：行号 + 文本内容。
fn render_preview_line(
    line_number: usize,
    line: &str,
    line_number_width: f32,
    syntax_spans: Vec<HighlightSpan>,
    theme: &AppTheme,
) -> impl IntoElement {
    let text_element = render_highlighted_preview_text(line, syntax_spans, theme);
    div()
        .h(px(FILE_PREVIEW_ROW_HEIGHT))
        .w_full()
        .flex()
        .items_center()
        .text_size(px(FILE_PREVIEW_FONT_SIZE))
        .line_height(px(FILE_PREVIEW_ROW_HEIGHT))
        .bg(rgb(theme.content))
        .hover(|this| this.bg(rgb(theme.current_line)))
        .child(
            div()
                .w(px(line_number_width))
                .h_full()
                .flex_none()
                .pr_3()
                .flex()
                .items_center()
                .justify_end()
                .border_r_1()
                .border_color(rgb(theme.border))
                .bg(rgb(theme.side_bar))
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
}

/// 把纯逻辑高亮范围转换为 GPUI 文本样式。
///
/// 参数说明：
/// - `line`：当前展示行。
/// - `spans`：高亮器生成的不重叠 UTF-8 字节范围。
/// - `theme`：当前窗口主题。
///
/// 返回值：没有 token 时返回普通文本，有 token 时返回带主题色的 `StyledText`。
fn render_highlighted_preview_text(
    line: &str,
    spans: Vec<HighlightSpan>,
    theme: &AppTheme,
) -> AnyElement {
    if spans.is_empty() {
        return line.to_string().into_any_element();
    }
    let highlights: Vec<_> = spans
        .into_iter()
        .map(|span| {
            (
                span.range,
                HighlightStyle {
                    color: Some(
                        rgb(color_for_highlight_token(
                            span.kind,
                            theme,
                            HighlightColorContext::FilePreview,
                        ))
                        .into(),
                    ),
                    ..Default::default()
                },
            )
        })
        .collect();
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
                .bg(rgb(theme.side_bar))
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

#[cfg(test)]
mod tests {
    use super::clamp_preview_line_range;

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
}
