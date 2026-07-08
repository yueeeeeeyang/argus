//! 文件职责：提供自绘滚动条滑块几何指标的共享计算。
//! 创建日期：2026-07-07
//! 修改日期：2026-07-07
//! 作者：Argus 开发团队
//! 主要功能：根据视口、内容长度和当前滚动量计算滑块位置与拖拽换算，供各处自绘滚动条复用，
//!          消除关键字历史、线程详情、Runtime 表格、Jstack 被动滚动条之间的重复实现。

use gpui::{Pixels, px};

/// 自绘滚动条滑块的几何指标。
///
/// 关键字历史下拉、Jstack 线程详情、Runtime 表格、Jstack 被动滚动条共用此结构：它们的
/// 渲染与拖拽交互各自不同，但滑块几何计算完全一致。日志正文与终端滚动条因几何模型不同
/// （横向 gutter / 行偏移而非像素偏移）未复用此结构。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollbarMetrics {
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
pub fn scrollbar_metrics(
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
    let thumb_length = ((viewport_length / content_length) * track_length)
        .clamp(px(min_thumb), track_length);
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
pub fn scrollbar_scroll_for_drag(
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
