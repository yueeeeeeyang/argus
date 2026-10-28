//! 文件职责：提供窗口内通用模态遮罩与容器组件。
//! 创建日期：2026-06-09
//! 修改日期：2026-10-28
//! 作者：Argus 开发团队
//! 主要功能：统一模态层遮罩、圆角容器、阴影和鼠标事件阻断能力。

use crate::theme::AppTheme;
use gpui::{
    AnyElement, Context, IntoElement, Pixels, Point, SharedString, div, prelude::*, px, rgb, rgba,
};

/// 通用模态框参数，调用方只负责提供内容元素。
pub(crate) struct ModalDialog {
    /// 模态遮罩元素 ID，便于测试定位和调试。
    pub overlay_id: &'static str,
    /// 模态容器元素 ID，便于测试定位和调试。
    pub container_id: &'static str,
    /// 模态容器宽度。
    pub width: f32,
    /// 模态容器高度。
    pub height: f32,
    /// 模态框内部内容。
    pub content: AnyElement,
}

/// 模态框可选外观与位置参数；默认无边框且保持居中。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ModalFrame {
    /// 容器边框颜色；`None` 表示不绘制边框。
    pub border_color: Option<u32>,
    /// 相对居中位置的拖动偏移。
    pub content_offset: Point<Pixels>,
}

/// 模态框内部内容与圆角外壳之间的安全内边距。
///
/// 取 0：内容根节点自身带圆角与灰色边框，内缩会在灰色边框线之外再露出一圈
/// 深色带，让边框看起来不是模态框的最外层。
///
/// **圆角约定（新增弹窗必须遵守）**：GPUI 的内容裁剪只支持矩形，`overflow_hidden`
/// 挡不住方角，因此内容根节点必须自带与外壳一致的圆角；任何贴到外壳边缘的
/// 子元素（左侧导航、全宽色带、整块背景）也必须自带对应边的圆角，否则会在
/// 圆角位置露出尖角。若贴边子元素与父级同色，优先直接删除它的背景（父级已经
/// 绘制过），而不是补圆角：重复绘制背景既无视觉效果，又会在圆角外溢出方角
/// （来源选择器侧栏标题与位置列表曾因此露出直角缺口）。
///
/// **高度约定（新增弹窗必须遵守）**：模态框内容通常是 flex 列布局，固定高度的
/// 标题栏、底栏必须标记 `flex_none`，可伸缩区域自己负责滚动；否则内容变长时
/// 弹性布局会压缩标题栏，让标题贴住上边框。
const MODAL_CONTENT_INSET: f32 = 0.0;

/// 模态框内容底部内边距。
///
/// 取 19：56px 标题栏内 18px 标题上下各留 19px；底部沿用同一距离，
/// 保证“标题到上边框”和“按钮到下边框”视觉一致，避免按钮贴边或底部大片空白。
pub(crate) const MODAL_BOTTOM_PADDING: f32 = 19.0;

/// 模态框底部主操作按钮高度。
pub(crate) const MODAL_ACTION_HEIGHT: f32 = 30.0;

/// 表单输入行高度（`InputSize::Regular` 输入框高度）。
pub(crate) const MODAL_FORM_ROW_HEIGHT: f32 = 30.0;

/// 表单行与行、行与提示之间的间距。
pub(crate) const MODAL_FORM_ROW_GAP: f32 = 12.0;

/// 表单单行提示（说明、警告、校验错误）的固定高度。
pub(crate) const MODAL_FORM_NOTE_HEIGHT: f32 = 16.0;

/// 渲染通用模态遮罩与容器。
///
/// 参数说明：
/// - `dialog`：模态框布局参数和内容。
/// - `theme`：当前主题令牌，用于计算遮罩和容器背景。
/// - `cx`：应用上下文，用于阻断遮罩层鼠标事件继续传递。
///
/// 返回值：覆盖整个窗口的 GPUI 元素树。
pub(crate) fn render_modal_dialog<T>(
    dialog: ModalDialog,
    theme: AppTheme,
    cx: &mut Context<T>,
) -> impl IntoElement
where
    T: 'static,
{
    render_modal_dialog_with_frame(dialog, ModalFrame::default(), theme, cx)
}

/// 渲染带可选边框与拖拽偏移的通用模态遮罩与容器。
///
/// 参数说明：
/// - `dialog`：模态框布局参数和内容。
/// - `frame`：容器边框与拖动偏移；默认值等价于无边框、居中显示。
/// - `theme`：当前主题令牌，用于计算遮罩和容器背景。
/// - `cx`：应用上下文，用于阻断遮罩层鼠标事件继续传递。
pub(crate) fn render_modal_dialog_with_frame<T>(
    dialog: ModalDialog,
    frame: ModalFrame,
    theme: AppTheme,
    cx: &mut Context<T>,
) -> impl IntoElement
where
    T: 'static,
{
    div()
        .id(SharedString::from(dialog.overlay_id))
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .bg(rgba(theme.modal_overlay))
        .occlude()
        .on_click(cx.listener(|_, _, _, cx| {
            cx.stop_propagation();
        }))
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .id(SharedString::from(dialog.container_id))
                .debug_selector({
                    let container_id = dialog.container_id;
                    move || container_id.to_string()
                })
                .occlude()
                .child(
                    div()
                        // 几何回归测试按“外壳”定位真实弹窗边界；容器 ID 指向全窗口居中层。
                        .debug_selector({
                            let shell_id = format!("{}-shell", dialog.container_id);
                            move || shell_id.clone()
                        })
                        .w(px(dialog.width))
                        .h(px(dialog.height))
                        .p(px(MODAL_CONTENT_INSET))
                        .rounded_lg()
                        .bg(rgb(theme.content))
                        .shadow_lg()
                        .when_some(frame.border_color, |this, color| {
                            this.border_1().border_color(rgb(color))
                        })
                        // 相对偏移不参与居中布局，仅平移视觉位置，便于模态框在窗口内拖动。
                        .relative()
                        .left(frame.content_offset.x)
                        .top(frame.content_offset.y)
                        .occlude()
                        .child(
                            div()
                                .size_full()
                                .flex()
                                .overflow_hidden()
                                .rounded_lg()
                                .occlude()
                                .child(dialog.content),
                        ),
                ),
        )
}
