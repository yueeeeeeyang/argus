//! 文件职责：为独立预览正文提供随悬停立即显隐的横纵滚动条和拖拽交互。

use super::*;
use crate::ui::components::scrollbar::{
    ScrollbarMetrics, scrollbar_metrics, scrollbar_scroll_for_drag,
};
use gpui::MouseExitEvent;

/// 与目录树相同的细滑块样式；两端保留空间，避免横纵滑块在角落重叠。
const PREVIEW_SCROLLBAR_THICKNESS: f32 = 4.0;
const PREVIEW_SCROLLBAR_EDGE_INSET: f32 = 1.0;
const PREVIEW_SCROLLBAR_PADDING: f32 = 5.0;
const PREVIEW_SCROLLBAR_MIN_THUMB: f32 = 36.0;

/// 预览滚动条所属坐标轴。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PreviewScrollbarAxis {
    Vertical,
    Horizontal,
}

/// 只记录被按住的滑块和指针相对滑块起点，拖动期间始终使用最新视口几何。
#[derive(Clone, Copy, Debug)]
pub(super) struct PreviewScrollbarDrag {
    axis: PreviewScrollbarAxis,
    cursor_offset: Pixels,
}

impl FilePreviewWindow {
    /// 内容不溢出或指针已离开正文时不创建滑块，避免透明滑块拦截正文输入。
    pub(super) fn render_preview_scrollbars(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if !self.is_body_hovered {
            return Vec::new();
        }
        [
            PreviewScrollbarAxis::Vertical,
            PreviewScrollbarAxis::Horizontal,
        ]
        .into_iter()
        .filter_map(|axis| {
            self.preview_scrollbar_metrics(axis)
                .map(|metrics| self.render_preview_scrollbar_thumb(axis, metrics, cx))
        })
        .collect()
    }

    /// 共享目录树与分析表格的滑块几何算法，两条轴均使用虚拟列表实际内容长度。
    fn preview_scrollbar_metrics(&self, axis: PreviewScrollbarAxis) -> Option<ScrollbarMetrics> {
        let state = self.scroll.0.as_ref().borrow();
        let size = state.last_item_size?;
        let viewport = state.base_handle.bounds().size;
        let offset = state.base_handle.offset();
        let (visible, content, offset) = match axis {
            PreviewScrollbarAxis::Vertical => (viewport.height, size.contents.height, -offset.y),
            PreviewScrollbarAxis::Horizontal => (viewport.width, size.contents.width, -offset.x),
        };
        let track_length = (visible - px(PREVIEW_SCROLLBAR_PADDING * 2.0)).max(px(1.0));
        let min_thumb = PREVIEW_SCROLLBAR_MIN_THUMB.min(f32::from(track_length));
        scrollbar_metrics(
            visible,
            content,
            offset,
            PREVIEW_SCROLLBAR_PADDING,
            min_thumb,
        )
    }

    /// 按住滑块不改变正文选区和搜索框焦点，只结束可能遗留的正文拖选。
    fn begin_preview_scrollbar_drag(
        &mut self,
        axis: PreviewScrollbarAxis,
        position: Point<Pixels>,
    ) {
        let Some(metrics) = self.preview_scrollbar_metrics(axis) else {
            return;
        };
        let bounds = self.scroll.0.as_ref().borrow().base_handle.bounds();
        let relative_pointer = match axis {
            PreviewScrollbarAxis::Vertical => position.y - bounds.top(),
            PreviewScrollbarAxis::Horizontal => position.x - bounds.left(),
        };
        self.finish_preview_text_selection();
        self.scrollbar_drag = Some(PreviewScrollbarDrag {
            axis,
            cursor_offset: relative_pointer - metrics.thumb_start,
        });
    }

    /// 拖动只修改目标轴；位置允许越出视口，最后由共享公式夹到有效范围。
    fn move_preview_scrollbar_drag(&mut self, position: Point<Pixels>) -> bool {
        let Some(drag) = self.scrollbar_drag else {
            return false;
        };
        let Some(metrics) = self.preview_scrollbar_metrics(drag.axis) else {
            self.scrollbar_drag = None;
            return false;
        };
        let handle = self.scroll.0.as_ref().borrow().base_handle.clone();
        let bounds = handle.bounds();
        let offset = handle.offset();
        let relative_pointer = match drag.axis {
            PreviewScrollbarAxis::Vertical => position.y - bounds.top(),
            PreviewScrollbarAxis::Horizontal => position.x - bounds.left(),
        };
        let scroll = scrollbar_scroll_for_drag(relative_pointer, drag.cursor_offset, &metrics);
        let next = match drag.axis {
            PreviewScrollbarAxis::Vertical => point(offset.x, -scroll),
            PreviewScrollbarAxis::Horizontal => point(-scroll, offset.y),
        };
        if next == offset {
            return false;
        }
        handle.set_offset(next);
        true
    }

    /// 细条直接覆盖正文边缘，滚轮继续交给下层虚拟列表。
    fn render_preview_scrollbar_thumb(
        &self,
        axis: PreviewScrollbarAxis,
        metrics: ScrollbarMetrics,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = match axis {
            PreviewScrollbarAxis::Vertical => "file-preview-scrollbar-vertical",
            PreviewScrollbarAxis::Horizontal => "file-preview-scrollbar-horizontal",
        };
        let thumb = div()
            .id(id)
            .debug_selector(move || id.to_string())
            .absolute()
            .rounded_lg()
            .bg(rgb(self.theme.foreground_muted))
            .opacity(0.45)
            .block_mouse_except_scroll()
            .cursor_default()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |view, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    view.begin_preview_scrollbar_drag(axis, event.position);
                    cx.notify();
                }),
            )
            .on_click(|_, _, cx| cx.stop_propagation());
        match axis {
            PreviewScrollbarAxis::Vertical => thumb
                .top(metrics.thumb_start)
                .right(px(PREVIEW_SCROLLBAR_EDGE_INSET))
                .w(px(PREVIEW_SCROLLBAR_THICKNESS))
                .h(metrics.thumb_length),
            PreviewScrollbarAxis::Horizontal => thumb
                .left(metrics.thumb_start)
                .bottom(px(PREVIEW_SCROLLBAR_EDGE_INSET))
                .h(px(PREVIEW_SCROLLBAR_THICKNESS))
                .w(metrics.thumb_length),
        }
        .into_any_element()
    }
}

/// 接收窗口级离开与拖拽事件，跨窗口移动无需等待正文再次收到鼠标移动或失去焦点。
pub(super) fn render_preview_scrollbar_sensor(cx: &mut Context<FilePreviewWindow>) -> AnyElement {
    let entity = cx.entity();
    canvas(
        |_, _, _| (),
        move |_, _, window: &mut Window, _| {
            window.on_mouse_event({
                let entity = entity.clone();
                move |_: &MouseExitEvent, phase, _, cx| {
                    if !phase.capture() {
                        return;
                    }
                    entity.update(cx, |view, cx| {
                        if view.is_body_hovered {
                            view.is_body_hovered = false;
                            cx.notify();
                        }
                    });
                }
            });
            window.on_mouse_event({
                let entity = entity.clone();
                move |event: &MouseMoveEvent, phase, _, cx| {
                    if !phase.bubble() || entity.read(cx).scrollbar_drag.is_none() {
                        return;
                    }
                    // 保留正文 hover 监听的冒泡机会，拖动离开视口时滑块也必须立即隐藏。
                    entity.update(cx, |view, cx| {
                        if event.dragging() {
                            if view.move_preview_scrollbar_drag(event.position) {
                                cx.notify();
                            }
                        } else {
                            view.scrollbar_drag = None;
                            cx.notify();
                        }
                    });
                }
            });
            window.on_mouse_event({
                let entity = entity.clone();
                move |event: &MouseUpEvent, phase, _, cx| {
                    if !phase.bubble() || event.button != MouseButton::Left {
                        return;
                    }
                    entity.update(cx, |view, cx| {
                        if view.scrollbar_drag.take().is_some() {
                            cx.stop_propagation();
                            cx.notify();
                        }
                    });
                }
            });
        },
    )
    .absolute()
    .size_full()
    .into_any_element()
}
