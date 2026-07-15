//! 文件职责：渲染远程文件预览独立窗口。
//! 创建日期：2026-07-03
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：在透明标题栏窗口中展示远程普通文件的文本内容，并提示二进制或读取失败。

use std::ops::Range;

use gpui::{
    AnyElement, Context, Entity, FocusHandle, FontWeight, IntoElement, Render, Subscription,
    UniformListScrollHandle, Window, div, prelude::*, px, rgb, uniform_list,
};

use crate::app::{ArgusApp, log_viewer_line_number_width, observe_app_theme};
use crate::fonts::{ARGUS_LOG_FONT_FAMILY, ARGUS_UI_FONT_FAMILY};
use crate::remote::remote_file::{FilePreviewContent, REMOTE_FILE_PREVIEW_MAX_READ};
use crate::theme::AppTheme;
use crate::ui::components::centered_message::render_centered_message;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::window_title_bar::render_window_title_bar;

/// 预览窗口标题栏高度；预览窗口比独立设置窗口更小，标题栏相应更紧凑。
const FILE_PREVIEW_HEADER_HEIGHT: f32 = 44.0;
/// 预览窗口标题图标尺寸。
const FILE_PREVIEW_TITLE_ICON_SIZE: f32 = 16.0;
/// 预览正文行高，保持与日志阅读区一致的高密度展示。
const FILE_PREVIEW_ROW_HEIGHT: f32 = 20.0;
/// 预览正文字号。
const FILE_PREVIEW_FONT_SIZE: f32 = 12.0;

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
    /// 预览正文状态。
    body: FilePreviewBody,
    /// 正文滚动句柄。
    scroll: UniformListScrollHandle,
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
        let _app_observer = observe_app_theme(cx, &app, theme.clone(), |view, theme, _| {
            view.theme = theme.clone();
        });

        Self {
            theme,
            file_name,
            body,
            scroll: UniformListScrollHandle::new(),
            root_focus: cx.focus_handle(),
            _app_observer,
        }
    }

    /// 渲染预览窗口标题栏。
    fn render_header(&self) -> impl IntoElement {
        let theme = self.theme.clone();
        let file_name = self.file_name.clone();
        let notice = match &self.body {
            FilePreviewBody::Text { truncated, .. } if *truncated => Some(format!(
                "仅显示前 {} KB",
                REMOTE_FILE_PREVIEW_MAX_READ / 1024
            )),
            _ => None,
        };

        // 左上角：文件图标 + 文件名（过长截断）+ 截断提示。
        let left = div()
            .flex()
            .items_center()
            .gap_2()
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
            )
            .when_some(notice, |this, notice| {
                this.child(
                    div()
                        .flex_none()
                        .text_size(px(12.0))
                        .text_color(rgb(theme.foreground_muted))
                        .child(notice),
                )
            });

        // 标题栏骨架（高度、分隔线、关闭按钮）由共享组件统一，避免多窗口漂移。
        render_window_title_bar(
            "file-preview-window-close",
            "关闭预览",
            FILE_PREVIEW_HEADER_HEIGHT,
            true,
            &self.theme,
            left,
            move |_, window, _| {
                window.remove_window();
            },
        )
    }

    /// 渲染预览正文。
    fn render_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        match &self.body {
            FilePreviewBody::Text { lines, .. } => {
                // 空文件不应创建 0 行虚拟列表；部分平台在首次布局时会为其生成无效可见区间。
                if lines.is_empty() {
                    return render_centered_message("文件内容为空", &self.theme, true);
                }
                let line_count = lines.len();
                // 行号栏宽度随行数自适应（复用日志阅读区算法），避免固定宽度在行号过多时截断。
                let line_number_width = log_viewer_line_number_width(line_count);
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
                                render_preview_line(
                                    line_number,
                                    line,
                                    line_number_width,
                                    &this.theme,
                                )
                                .into_any_element()
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .size_full()
                .track_scroll(self.scroll.clone())
                .into_any_element()
            }
            FilePreviewBody::Binary => {
                render_centered_message("二进制文件无法预览", &self.theme, true)
            }
            FilePreviewBody::Error(message) => render_centered_message(message, &self.theme, true),
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
    }
}

/// 渲染预览正文单行：行号 + 文本内容。
fn render_preview_line(
    line_number: usize,
    line: &str,
    line_number_width: f32,
    theme: &AppTheme,
) -> impl IntoElement {
    div()
        .h(px(FILE_PREVIEW_ROW_HEIGHT))
        .w_full()
        .flex()
        .items_center()
        .text_size(px(FILE_PREVIEW_FONT_SIZE))
        .line_height(px(FILE_PREVIEW_ROW_HEIGHT))
        .child(
            div()
                .w(px(line_number_width))
                .flex_none()
                .pr_2()
                .text_color(rgb(theme.foreground_muted))
                .truncate()
                .child(line_number.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .px_3()
                .text_color(rgb(theme.foreground))
                .truncate()
                .child(line.to_string()),
        )
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
