//! 文件职责：提供拖拽文本选择时的视口边缘自动滚动计算。
//! 创建日期：2026-09-23
//! 修改日期：2026-09-29
//! 作者：Argus 开发团队
//! 主要功能：统一各视图的滚动强度、步长和偏移推进规则，同时支持横向与纵向两个轴向。

/// 单帧自动滚动的最大步长（像素），避免滚动跳跃感。
pub(crate) const SELECTION_AUTOSCROLL_MAX_STEP: f32 = 12.0;

/// 计算单轴拖拽指针的自动滚动强度。
///
/// 参数说明：
/// - `pointer`：指针在该轴向上的窗口坐标。
/// - `viewport_start`：视口在该轴向的起始坐标。
/// - `viewport_end`：视口在该轴向的结束坐标。
///
/// 返回值：负值表示向起始方向滚动、正值表示向结束方向滚动、0 表示指针仍在视口内；
/// 视口尺寸退化（结束不大于起始）时返回 0。只有指针真正越过视口边缘才产生强度，
/// 避免指针尚在视口内时内容就开始滚动、选择手感漂移。
pub(crate) fn selection_autoscroll_intensity(
    pointer: f32,
    viewport_start: f32,
    viewport_end: f32,
) -> f32 {
    if viewport_end <= viewport_start {
        return 0.0;
    }
    if pointer < viewport_start {
        pointer - viewport_start
    } else if pointer > viewport_end {
        pointer - viewport_end
    } else {
        0.0
    }
}

/// 按滚动强度计算每帧滚动像素；强度越大滚动越快，并限制在最大步长内。
pub(crate) fn selection_autoscroll_step_px(intensity: f32) -> f32 {
    (intensity.abs() * 0.4)
        .clamp(1.0, SELECTION_AUTOSCROLL_MAX_STEP)
        .copysign(intensity)
}

/// 推进使用正数偏移的滚动位置（如分页日志的 `top_px`/`left_px`）。
///
/// 参数说明：
/// - `current`：当前滚动位置，非负。
/// - `step`：本帧滚动像素，正数向结束方向滚动。
/// - `max_scroll`：该轴向的最大滚动位置。
pub(crate) fn advance_positive_scroll(current: f64, step: f32, max_scroll: f64) -> f64 {
    (current + f64::from(step)).clamp(0.0, max_scroll)
}

/// 推进使用负数偏移的滚动位置（GPUI `ScrollHandle::set_offset` 语义）。
///
/// 参数说明：
/// - `current`：当前偏移，范围为 `[-max_scroll, 0]`。
/// - `step`：本帧滚动像素，正数向结束方向滚动（偏移变小）。
/// - `max_scroll`：该轴向的最大滚动距离。
pub(crate) fn advance_negative_scroll(current: f32, step: f32, max_scroll: f32) -> f32 {
    (current - step).clamp(-max_scroll, 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证视口内部（含贴近边缘）强度为 0，越过边缘后强度等于带符号的越界距离。
    #[test]
    fn intensity_is_zero_inside_viewport_and_grows_beyond_edges() {
        assert_eq!(selection_autoscroll_intensity(200.0, 100.0, 500.0), 0.0);
        assert_eq!(selection_autoscroll_intensity(100.5, 100.0, 500.0), 0.0);
        assert_eq!(selection_autoscroll_intensity(499.5, 100.0, 500.0), 0.0);

        assert_eq!(selection_autoscroll_intensity(95.0, 100.0, 500.0), -5.0);
        assert_eq!(selection_autoscroll_intensity(530.0, 100.0, 500.0), 30.0);

        let start_near = selection_autoscroll_intensity(90.0, 100.0, 500.0);
        let start_far = selection_autoscroll_intensity(50.0, 100.0, 500.0);
        assert!(start_near < 0.0);
        assert!(start_far < start_near);
    }

    /// 验证退化的视口尺寸不会产生自动滚动强度。
    #[test]
    fn intensity_handles_degenerate_viewport() {
        assert_eq!(selection_autoscroll_intensity(10.0, 100.0, 100.0), 0.0);
        assert_eq!(selection_autoscroll_intensity(10.0, 100.0, 50.0), 0.0);
    }

    /// 验证步长保底 1px、按强度等比推进、上限受最大步长约束，并保留滚动方向。
    #[test]
    fn step_clamps_speed_range_and_keeps_direction() {
        assert_eq!(selection_autoscroll_step_px(1.0), 1.0);
        assert_eq!(selection_autoscroll_step_px(-1.0), -1.0);
        assert_eq!(selection_autoscroll_step_px(10.0), 4.0);
        assert_eq!(selection_autoscroll_step_px(-10.0), -4.0);
        assert_eq!(
            selection_autoscroll_step_px(1000.0),
            SELECTION_AUTOSCROLL_MAX_STEP
        );
        assert_eq!(
            selection_autoscroll_step_px(-1000.0),
            -SELECTION_AUTOSCROLL_MAX_STEP
        );
    }

    /// 验证正数偏移推进不会越出滚动范围。
    #[test]
    fn positive_scroll_advance_stays_in_range() {
        assert_eq!(advance_positive_scroll(10.0, 5.0, 100.0), 15.0);
        assert_eq!(advance_positive_scroll(98.0, 5.0, 100.0), 100.0);
        assert_eq!(advance_positive_scroll(2.0, -5.0, 100.0), 0.0);
        assert_eq!(advance_positive_scroll(0.0, -5.0, 0.0), 0.0);
    }

    /// 验证负数偏移推进不会越出滚动范围，且正步长向结束方向滚动。
    #[test]
    fn negative_scroll_advance_stays_in_range() {
        assert_eq!(advance_negative_scroll(-10.0, 5.0, 100.0), -15.0);
        assert_eq!(advance_negative_scroll(-98.0, 5.0, 100.0), -100.0);
        assert_eq!(advance_negative_scroll(-2.0, -5.0, 100.0), 0.0);
        assert_eq!(advance_negative_scroll(0.0, -5.0, 0.0), 0.0);
    }
}
