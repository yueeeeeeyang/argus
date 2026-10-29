//! 文件职责：提供自绘滚动条滑块几何指标的共享计算。
//! 创建日期：2026-07-07
//! 修改日期：2026-10-29
//! 作者：Argus 开发团队
//! 主要功能：计算滚动条几何、支持悬停显隐和拖拽，并提供普通滚动容器与虚拟列表的通用滑块。

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};

use gpui::{
    AnyElement, IntoElement, ListState, MouseButton, MouseDownEvent, MouseExitEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, ScrollHandle, Styled, Window, canvas, div, point,
    prelude::*, px, rgb,
};

use crate::theme::AppTheme;

/// 自绘滚动条滑块的几何指标。
///
/// 关键字历史下拉、Jstack 线程详情、Runtime 表格、Jstack 被动滚动条共用此结构：它们的
/// 渲染与拖拽交互各自不同，但滑块几何计算完全一致。日志正文与终端滚动条因几何模型不同
/// （横向 gutter / 行偏移而非像素偏移）未复用此结构。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ScrollbarMetrics {
    /// 滑块起始位置（相对滚动容器顶部或左侧）。
    pub thumb_start: Pixels,
    /// 滑块长度。
    pub thumb_length: Pixels,
    /// 轨道起始位置。
    pub track_start: Pixels,
    /// 轨道长度。
    pub track_length: Pixels,
    /// 最大滚动距离。
    pub max_scroll: Pixels,
}

/// 根据视口、内容长度和当前滚动量计算滑块指标；内容未溢出视口时返回 `None`。
///
/// `padding` 为轨道两端留白，`min_thumb` 为滑块最小长度，由各调用方按自身常量传入。
pub(crate) fn scrollbar_metrics(
    viewport_length: Pixels,
    content_length: Pixels,
    scroll_offset: Pixels,
    padding: f32,
    min_thumb: f32,
) -> Option<ScrollbarMetrics> {
    if viewport_length == px(0.0) || content_length <= viewport_length {
        return None;
    }

    let track_padding = px(padding);
    let track_length = (viewport_length - track_padding * 2.0).max(px(1.0));
    let thumb_length =
        ((viewport_length / content_length) * track_length).clamp(px(min_thumb), track_length);
    let max_scroll = (content_length - viewport_length).max(px(1.0));
    let movable_length = (track_length - thumb_length).max(px(0.0));
    let scroll_ratio = (scroll_offset / max_scroll).clamp(0.0, 1.0);
    let thumb_start = track_padding + movable_length * scroll_ratio;

    Some(ScrollbarMetrics {
        thumb_start,
        thumb_length,
        track_start: track_padding,
        track_length,
        max_scroll,
    })
}

/// 根据拖拽中的鼠标位置换算目标滚动距离。
///
/// `pointer` 为鼠标相对滚动容器顶部/左侧的位置，`cursor_offset` 为按下时鼠标落在滑块内的
/// 偏移量。返回值为目标滚动偏移（正数，调用方按轴向取负后写入 `ScrollHandle`）。
pub(crate) fn scrollbar_scroll_for_drag(
    pointer: Pixels,
    cursor_offset: Pixels,
    metrics: &ScrollbarMetrics,
) -> Pixels {
    let movable_length = (metrics.track_length - metrics.thumb_length).max(px(1.0));
    let thumb_start =
        (pointer - cursor_offset).clamp(metrics.track_start, metrics.track_start + movable_length);
    let ratio = (thumb_start - metrics.track_start) / movable_length;
    metrics.max_scroll * ratio
}

/// 当前各滚动区域是否处于鼠标悬停状态。
static SCROLLBAR_HOVER_STATES: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();

/// 更新某个滚动区域的悬停状态；返回状态是否发生变化。
pub(crate) fn set_scrollbar_hover(id: &str, hovered: bool) -> bool {
    let states = SCROLLBAR_HOVER_STATES.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut states) = states.lock() else {
        return false;
    };
    let previous = states.insert(id.to_string(), hovered).unwrap_or(false);
    previous != hovered
}

/// 返回某个滚动区域当前是否处于鼠标悬停状态。
pub(crate) fn scrollbar_hovered(id: &str) -> bool {
    SCROLLBAR_HOVER_STATES
        .get()
        .and_then(|states| states.lock().ok())
        .and_then(|states| states.get(id).copied())
        .unwrap_or(false)
}

/// 构造滚动区域悬停监听器：进入区域显示滚动条，离开立即隐藏。
pub(crate) fn scroll_area_hover_handler(
    id: String,
) -> impl Fn(&bool, &mut gpui::Window, &mut gpui::App) + 'static {
    move |is_hovered, window, _| {
        if set_scrollbar_hover(&id, *is_hovered) {
            window.refresh();
        }
    }
}

/// 按悬停状态设置滚动条透明度；鼠标离开滚动区域时立即隐藏。
pub(crate) fn with_scrollbar_visibility<E: Styled>(thumb: E, id: &str, base_opacity: f32) -> E {
    thumb.opacity(if scrollbar_hovered(id) {
        base_opacity
    } else {
        0.0
    })
}

/// 自绘纵向滚动条宽度。
const SCROLLBAR_THICKNESS: f32 = 5.0;
/// 滚动条贴住视口右缘的间距。
const SCROLLBAR_EDGE_INSET: f32 = 1.0;
/// 滚动条轨道两端留白。
const SCROLLBAR_TRACK_PADDING: f32 = 4.0;
/// 滚动条滑块最小长度。
const SCROLLBAR_MIN_THUMB: f32 = 32.0;
/// 滑块默认透明度。
const SCROLLBAR_THUMB_OPACITY: f32 = 0.5;

/// 当前正在拖拽的滚动条及指针相对滑块起点的偏移。
static SCROLLBAR_DRAG_OFFSETS: OnceLock<Mutex<HashMap<String, f32>>> = OnceLock::new();

/// 返回滚动条拖拽偏移表。
fn scrollbar_drag_offsets() -> &'static Mutex<HashMap<String, f32>> {
    SCROLLBAR_DRAG_OFFSETS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 更新指定滚动条的拖拽偏移；`None` 表示结束拖拽。
fn set_scrollbar_drag_offset(id: &str, offset: Option<f32>) {
    let Ok(mut offsets) = scrollbar_drag_offsets().lock() else {
        return;
    };
    match offset {
        Some(offset) => {
            offsets.insert(id.to_string(), offset);
        }
        None => {
            offsets.remove(id);
        }
    }
}

/// 返回指定滚动条当前的拖拽偏移。
fn scrollbar_drag_offset(id: &str) -> Option<f32> {
    scrollbar_drag_offsets()
        .lock()
        .ok()
        .and_then(|offsets| offsets.get(id).copied())
}

/// 渲染普通滚动容器的纵向滚动条；仅在悬停或拖拽时创建，避免透明滑块拦截鼠标。
pub(crate) fn render_scroll_handle_vertical_scrollbar(
    scroll_handle: ScrollHandle,
    hover_id: &'static str,
    thumb_id: &'static str,
    theme: &AppTheme,
) -> Option<AnyElement> {
    if !scrollbar_hovered(hover_id) && scrollbar_drag_offset(thumb_id).is_none() {
        return None;
    }
    let bounds = scroll_handle.bounds();
    let max_offset = scroll_handle.max_offset();
    let offset = scroll_handle.offset();
    let metrics = scrollbar_metrics(
        bounds.size.height,
        bounds.size.height + max_offset.height,
        -offset.y,
        SCROLLBAR_TRACK_PADDING,
        SCROLLBAR_MIN_THUMB,
    )?;
    let handle = scroll_handle.clone();
    Some(render_vertical_scrollbar_thumb(
        thumb_id,
        metrics,
        bounds.top(),
        theme,
        move |scroll| {
            let current = handle.offset();
            handle.set_offset(point(current.x, -scroll));
        },
        || {},
        || {},
    ))
}

/// 渲染虚拟列表的纵向滚动条；几何数据直接取自 `ListState`。
pub(crate) fn render_list_vertical_scrollbar(
    list_state: ListState,
    hover_id: &'static str,
    thumb_id: &'static str,
    theme: &AppTheme,
) -> Option<AnyElement> {
    if !scrollbar_hovered(hover_id) && scrollbar_drag_offset(thumb_id).is_none() {
        return None;
    }
    let viewport = list_state.viewport_bounds();
    let max_scroll = list_state.max_offset_for_scrollbar().height;
    if viewport.size.height <= px(0.0) || max_scroll <= px(0.0) {
        return None;
    }
    let current_scroll = -list_state.scroll_px_offset_for_scrollbar().y;
    let metrics = scrollbar_metrics(
        viewport.size.height,
        viewport.size.height + max_scroll,
        current_scroll,
        SCROLLBAR_TRACK_PADDING,
        SCROLLBAR_MIN_THUMB,
    )?;
    let drag_state = list_state.clone();
    let drag_start_state = list_state.clone();
    let drag_end_state = list_state.clone();
    Some(render_vertical_scrollbar_thumb(
        thumb_id,
        metrics,
        viewport.top(),
        theme,
        move |scroll| {
            drag_state.set_offset_from_scrollbar(point(px(0.0), -scroll));
        },
        move || drag_start_state.scrollbar_drag_started(),
        move || drag_end_state.scrollbar_drag_ended(),
    ))
}

/// 渲染可拖拽纵向滑块；按下时记录指针在滑块内的偏移，移动时交给调用方滚动。
fn render_vertical_scrollbar_thumb<Drag, DragStart, DragEnd>(
    thumb_id: &'static str,
    metrics: ScrollbarMetrics,
    viewport_top: Pixels,
    theme: &AppTheme,
    on_drag: Drag,
    on_drag_start: DragStart,
    on_drag_end: DragEnd,
) -> AnyElement
where
    Drag: Fn(Pixels) + 'static,
    DragStart: Fn() + 'static,
    DragEnd: Fn() + 'static,
{
    let on_drag = Rc::new(on_drag);
    let on_drag_start = Rc::new(on_drag_start);
    let on_drag_end = Rc::new(on_drag_end);
    div()
        .id(thumb_id)
        .debug_selector(move || thumb_id.to_string())
        .absolute()
        .top(metrics.thumb_start)
        .right(px(SCROLLBAR_EDGE_INSET))
        .w(px(SCROLLBAR_THICKNESS))
        .h(metrics.thumb_length)
        .rounded_lg()
        .bg(rgb(theme.foreground_muted))
        .opacity(SCROLLBAR_THUMB_OPACITY)
        .block_mouse_except_scroll()
        .cursor_pointer()
        .child(
            canvas(
                |_, _, _| (),
                move |thumb_bounds, _, window: &mut Window, _| {
                    window.on_mouse_event({
                        let on_drag_start = on_drag_start.clone();
                        move |event: &MouseDownEvent, phase, _, cx| {
                            if !phase.bubble()
                                || event.button != MouseButton::Left
                                || !thumb_bounds.contains(&event.position)
                            {
                                return;
                            }
                            on_drag_start();
                            set_scrollbar_drag_offset(
                                thumb_id,
                                Some(f32::from(event.position.y - thumb_bounds.top())),
                            );
                            cx.stop_propagation();
                        }
                    });
                    window.on_mouse_event({
                        let on_drag = on_drag.clone();
                        move |event: &MouseMoveEvent, phase, _, cx| {
                            if !phase.bubble() || !event.dragging() {
                                return;
                            }
                            let Some(cursor_offset) = scrollbar_drag_offset(thumb_id) else {
                                return;
                            };
                            let pointer = event.position.y - viewport_top;
                            let scroll =
                                scrollbar_scroll_for_drag(pointer, px(cursor_offset), &metrics);
                            on_drag(scroll);
                            cx.stop_propagation();
                        }
                    });
                    window.on_mouse_event({
                        let on_drag_end = on_drag_end.clone();
                        move |_: &MouseExitEvent, phase, _, _| {
                            if !phase.capture() || scrollbar_drag_offset(thumb_id).is_none() {
                                return;
                            }
                            set_scrollbar_drag_offset(thumb_id, None);
                            on_drag_end();
                        }
                    });
                    window.on_mouse_event({
                        let on_drag_end = on_drag_end.clone();
                        move |event: &MouseUpEvent, phase, _, cx| {
                            if !phase.capture() || event.button != MouseButton::Left {
                                return;
                            }
                            if scrollbar_drag_offset(thumb_id).is_none() {
                                return;
                            }
                            set_scrollbar_drag_offset(thumb_id, None);
                            on_drag_end();
                            cx.stop_propagation();
                        }
                    });
                },
            )
            .size_full(),
        )
        .into_any_element()
}

/// 测试用普通滚动容器视图，验证 ScrollHandle 版滚动条的悬停显隐与拖拽。
#[cfg(test)]
struct ScrollHandleTestView {
    theme: AppTheme,
    scroll: ScrollHandle,
}

#[cfg(test)]
impl gpui::Render for ScrollHandleTestView {
    fn render(&mut self, _window: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .id("scroll-handle-test-area")
            .debug_selector(|| "scroll-handle-test-area".to_string())
            .relative()
            .w(px(300.0))
            .h(px(120.0))
            .bg(rgb(self.theme.content))
            .on_hover(scroll_area_hover_handler("scroll-handle-test".to_string()))
            .child(
                div()
                    .id("scroll-handle-test-content")
                    .size_full()
                    .overflow_hidden()
                    .overflow_y_scroll()
                    .scrollbar_width(px(0.0))
                    .track_scroll(&self.scroll)
                    .child(div().w_full().h(px(800.0)).bg(rgb(self.theme.background))),
            )
            .children(render_scroll_handle_vertical_scrollbar(
                self.scroll.clone(),
                "scroll-handle-test",
                "scroll-handle-test-thumb",
                &self.theme,
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ArgusApp;
    use crate::config::ConfigManager;
    use crate::config::paths::isolated_test_dir;
    use gpui::{Modifiers, MouseButton, TestAppContext, point, size};

    /// 验证拖拽偏移按滚动条独立记录，并能正确清除。
    #[test]
    fn scrollbar_drag_offset_tracks_each_bar() {
        set_scrollbar_drag_offset("scrollbar-drag-a", Some(12.5));
        assert_eq!(scrollbar_drag_offset("scrollbar-drag-a"), Some(12.5));
        set_scrollbar_drag_offset("scrollbar-drag-a", None);
        assert_eq!(scrollbar_drag_offset("scrollbar-drag-a"), None);
    }

    /// 验证悬停状态按滚动区域独立记录，并在状态变化时反馈。
    #[test]
    fn scrollbar_hover_state_tracks_each_area() {
        assert!(!set_scrollbar_hover("scrollbar-test-a", false));
        assert!(set_scrollbar_hover("scrollbar-test-a", true));
        assert!(scrollbar_hovered("scrollbar-test-a"));
        assert!(!scrollbar_hovered("scrollbar-test-b"));
        assert!(set_scrollbar_hover("scrollbar-test-a", false));
        assert!(!scrollbar_hovered("scrollbar-test-a"));
    }

    /// 验证普通滚动容器悬停显示滑块，并可拖拽改变滚动位置。
    #[gpui::test]
    fn scroll_handle_scrollbar_shows_on_hover_and_drags(cx: &mut TestAppContext) {
        let app = cx.new(|_| {
            ArgusApp::new_with_config_manager(ConfigManager::new(
                isolated_test_dir("scrollbar-handle").join("settings.toml"),
            ))
        });
        let theme = cx.read(|cx| app.read(cx).theme.clone());
        let (view, cx) = cx.add_window_view(move |_, _| ScrollHandleTestView {
            theme,
            scroll: ScrollHandle::new(),
        });
        cx.simulate_resize(size(px(500.0), px(300.0)));
        toggle_test_window_width(cx);
        let area = cx
            .debug_bounds("scroll-handle-test-area")
            .expect("应存在滚动区域");
        cx.simulate_mouse_move(area.center(), None, Modifiers::default());
        toggle_test_window_width(cx);

        assert!(scrollbar_hovered("scroll-handle-test"));
        let thumb = cx
            .debug_bounds("scroll-handle-test-thumb")
            .expect("悬停时应显示滚动条");
        let before = view.read_with(cx, |view, _| view.scroll.offset().y);
        cx.simulate_mouse_down(thumb.center(), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(
            point(thumb.center().x, area.bottom() - px(10.0)),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        cx.simulate_mouse_up(
            point(thumb.center().x, area.bottom() - px(10.0)),
            MouseButton::Left,
            Modifiers::default(),
        );
        let after = view.read_with(cx, |view, _| view.scroll.offset().y);
        assert_ne!(before, after, "拖拽滚动条应改变滚动位置");
    }

    /// 通过一像素窗口尺寸变化强制完成一次布局和绘制。
    fn toggle_test_window_width(cx: &mut gpui::VisualTestContext) {
        let viewport = cx.update(|window, _| window.viewport_size());
        let width = if viewport.width == px(500.0) {
            501.0
        } else {
            500.0
        };
        cx.simulate_resize(size(px(width), viewport.height));
    }
}
