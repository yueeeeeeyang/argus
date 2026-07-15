//! 文件职责：提供全局复用的加载旋转图标组件。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：统一来源树、日志读取等加载状态的旋转动画样式。

use crate::ui::components::icon::ArgusIcon;
use gpui::{
    Animation, AnimationExt as _, AnyElement, Transformation, percentage, prelude::*, px, rgb, svg,
};
use std::time::Duration;

/// 加载旋转动画单次旋转时长，保持来源树与内容区加载反馈一致。
const LOADING_SPINNER_DURATION_MS: u64 = 850;

/// 渲染统一加载旋转图标。
///
/// 参数说明：
/// - `animation_key`：动画实例键，同屏多个 spinner 需要使用不同键避免动画状态串扰。
/// - `color`：图标颜色，通常使用当前主题的次级文本色。
/// - `size`：图标尺寸，按所在 UI 区域传入。
///
/// 返回值：带无限旋转动画的 SVG 元素。
pub(crate) fn render_loading_spinner(
    animation_key: (&'static str, usize),
    color: u32,
    size: f32,
) -> AnyElement {
    svg()
        .path(ArgusIcon::Refresh.path())
        .size(px(size))
        .text_color(rgb(color))
        .with_animation(
            animation_key,
            Animation::new(Duration::from_millis(LOADING_SPINNER_DURATION_MS)).repeat(),
            |icon, progress| icon.with_transformation(Transformation::rotate(percentage(progress))),
        )
        .into_any_element()
}
