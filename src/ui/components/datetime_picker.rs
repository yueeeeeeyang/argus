//! 文件职责：提供自绘日期时间选择器组件。
//! 创建日期：2026-06-26
//! 修改日期：2026-06-26
//! 作者：Argus 开发团队
//! 主要功能：为 Runtime 日志过滤提供类似 Web 日期时间选择器的日历面板、时间微调和快捷按钮。

use crate::app::{
    ArgusApp, RuntimeDateTimePart, RuntimeDateTimeQuickAction, RuntimeFilterInputKind,
};
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use chrono::{Datelike, Duration, Local, NaiveDate};
use gpui::{
    Context, FontWeight, IntoElement, MouseButton, MouseDownEvent, SharedString, div, prelude::*,
    px, rgb,
};

/// 日期时间选择器浮层宽度，兼顾日历网格和时间操作按钮。
const DATETIME_PICKER_WIDTH: f32 = 304.0;
/// 日历单元格尺寸，保持 7 列网格稳定。
const DATETIME_DAY_CELL_SIZE: f32 = 34.0;
/// 星期标题。
const DATETIME_WEEKDAY_LABELS: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];

/// 日期时间选择器当前展示值。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DateTimePickerValue {
    /// 年。
    pub year: i32,
    /// 月。
    pub month: u32,
    /// 日。
    pub day: u32,
    /// 时。
    pub hour: u32,
    /// 分。
    pub minute: u32,
    /// 秒。
    pub second: u32,
}

/// 日期时间选择器中的单个日历日期。
#[derive(Clone, Debug, Eq, PartialEq)]
struct DateTimePickerDay {
    /// 日期。
    date: NaiveDate,
    /// 是否属于当前展示月份。
    is_current_month: bool,
    /// 是否是当前已选日期。
    is_selected: bool,
    /// 是否是今天。
    is_today: bool,
}

/// 渲染 Runtime 使用的日期时间选择器浮层。
pub fn render_datetime_picker(
    analysis_id: usize,
    input_kind: RuntimeFilterInputKind,
    title: &'static str,
    value: DateTimePickerValue,
    left: f32,
    top: f32,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    div()
        .id(SharedString::from(format!(
            "runtime-datetime-picker-{analysis_id}-{input_kind:?}"
        )))
        .absolute()
        .top(px(top))
        .left(px(left))
        .w(px(DATETIME_PICKER_WIDTH))
        .p_2()
        .flex()
        .flex_col()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.content))
        .shadow_lg()
        .occlude()
        .text_color(rgb(theme.foreground))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|_, _: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
            }),
        )
        .on_click(cx.listener(|_, _, _, cx| {
            cx.stop_propagation();
        }))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(render_month_nav_button(
                    format!("runtime-time-prev-month-{analysis_id}-{input_kind:?}"),
                    ArgusIcon::ArrowLeft,
                    theme,
                    cx.listener(move |app, _, _, cx| {
                        cx.stop_propagation();
                        app.adjust_runtime_filter_time(
                            analysis_id,
                            input_kind,
                            RuntimeDateTimePart::Month,
                            -1,
                            Some(cx),
                        );
                        cx.notify();
                    }),
                ))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .child(
                            div()
                                .text_size(px(12.0))
                                .line_height(px(15.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format!("{:04} 年 {:02} 月", value.year, value.month)),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .line_height(px(14.0))
                                .text_color(rgb(theme.foreground_muted))
                                .child(title),
                        ),
                )
                .child(render_month_nav_button(
                    format!("runtime-time-next-month-{analysis_id}-{input_kind:?}"),
                    ArgusIcon::ArrowRight,
                    theme,
                    cx.listener(move |app, _, _, cx| {
                        cx.stop_propagation();
                        app.adjust_runtime_filter_time(
                            analysis_id,
                            input_kind,
                            RuntimeDateTimePart::Month,
                            1,
                            Some(cx),
                        );
                        cx.notify();
                    }),
                )),
        )
        .child(render_calendar_weekdays(theme))
        .child(render_calendar_grid(
            analysis_id,
            input_kind,
            &value,
            theme,
            cx,
        ))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_1()
                .pt_1()
                .border_t_1()
                .border_color(rgb(theme.border))
                .child(
                    div()
                        .text_size(px(11.0))
                        .line_height(px(18.0))
                        .text_color(rgb(theme.foreground_muted))
                        .child("时间"),
                )
                .child(render_time_part_control(
                    analysis_id,
                    input_kind,
                    RuntimeDateTimePart::Hour,
                    format!("{:02}", value.hour),
                    theme,
                    cx,
                ))
                .child(render_time_part_separator(theme))
                .child(render_time_part_control(
                    analysis_id,
                    input_kind,
                    RuntimeDateTimePart::Minute,
                    format!("{:02}", value.minute),
                    theme,
                    cx,
                ))
                .child(render_time_part_separator(theme))
                .child(render_time_part_control(
                    analysis_id,
                    input_kind,
                    RuntimeDateTimePart::Second,
                    format!("{:02}", value.second),
                    theme,
                    cx,
                )),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(render_datetime_quick_button(
                    analysis_id,
                    input_kind,
                    RuntimeDateTimeQuickAction::TodayStart,
                    "今天开始",
                    theme,
                    cx,
                ))
                .child(render_datetime_quick_button(
                    analysis_id,
                    input_kind,
                    RuntimeDateTimeQuickAction::Now,
                    "现在",
                    theme,
                    cx,
                ))
                .child(render_datetime_quick_button(
                    analysis_id,
                    input_kind,
                    RuntimeDateTimeQuickAction::TodayEnd,
                    "今天结束",
                    theme,
                    cx,
                ))
                .child(render_datetime_quick_button(
                    analysis_id,
                    input_kind,
                    RuntimeDateTimeQuickAction::Clear,
                    "清空",
                    theme,
                    cx,
                ))
                .child(render_datetime_confirm_button(
                    analysis_id,
                    "确定",
                    theme,
                    cx,
                )),
        )
}

/// 渲染月份切换按钮。
fn render_month_nav_button(
    id: String,
    icon: ArgusIcon,
    theme: &AppTheme,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(id))
        .size(px(26.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .cursor_pointer()
        .hover(|this| this.bg(rgb(theme.selection)))
        .child(render_icon(icon, theme.foreground, 13.0))
        .on_click(on_click)
}

/// 渲染星期标题行。
fn render_calendar_weekdays(theme: &AppTheme) -> impl IntoElement + use<> {
    let mut row = div().flex().items_center().justify_between();
    for label in DATETIME_WEEKDAY_LABELS {
        row = row.child(
            div()
                .w(px(DATETIME_DAY_CELL_SIZE))
                .h(px(18.0))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.0))
                .line_height(px(18.0))
                .text_color(rgb(theme.foreground_muted))
                .child(label),
        );
    }
    row
}

/// 渲染日期网格，固定 6 行 7 列以避免月份切换时浮层高度跳动。
fn render_calendar_grid(
    analysis_id: usize,
    input_kind: RuntimeFilterInputKind,
    value: &DateTimePickerValue,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let days = datetime_picker_days(value);
    let mut grid = div().flex().flex_col().gap_1();
    for week in days.chunks(7) {
        let mut row = div().flex().items_center().justify_between();
        for day in week {
            row = row.child(render_calendar_day(
                analysis_id,
                input_kind,
                day.clone(),
                theme,
                cx,
            ));
        }
        grid = grid.child(row);
    }
    grid
}

/// 渲染单个日期单元格。
fn render_calendar_day(
    analysis_id: usize,
    input_kind: RuntimeFilterInputKind,
    day: DateTimePickerDay,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    let background = if day.is_selected {
        theme.info
    } else if day.is_today {
        theme.selection
    } else {
        theme.content
    };
    let foreground = if day.is_selected {
        theme.background
    } else if day.is_current_month {
        theme.foreground
    } else {
        theme.foreground_muted
    };
    let border = if day.is_selected || day.is_today {
        theme.info
    } else {
        theme.content
    };
    let year = day.date.year();
    let month = day.date.month();
    let date_day = day.date.day();

    div()
        .id(SharedString::from(format!(
            "runtime-time-day-{analysis_id}-{input_kind:?}-{year}-{month}-{date_day}"
        )))
        .w(px(DATETIME_DAY_CELL_SIZE))
        .h(px(28.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(border))
        .cursor_pointer()
        .bg(rgb(background))
        .text_size(px(12.0))
        .line_height(px(28.0))
        .text_color(rgb(foreground))
        .hover(|this| {
            this.bg(rgb(if day.is_selected {
                theme.info
            } else {
                theme.selection
            }))
        })
        .child(format!("{date_day}"))
        .on_click(cx.listener(move |app, _, _, cx| {
            cx.stop_propagation();
            app.set_runtime_filter_date(analysis_id, input_kind, year, month, date_day, Some(cx));
            cx.notify();
        }))
}

/// 构造当前月份的 6x7 日期矩阵，周一作为每周第一天。
fn datetime_picker_days(value: &DateTimePickerValue) -> Vec<DateTimePickerDay> {
    let selected_date = NaiveDate::from_ymd_opt(value.year, value.month, value.day)
        .unwrap_or_else(|| Local::now().date_naive());
    let month_start = NaiveDate::from_ymd_opt(selected_date.year(), selected_date.month(), 1)
        .unwrap_or(selected_date);
    let start_offset = month_start.weekday().num_days_from_monday() as i64;
    let grid_start = month_start - Duration::days(start_offset);
    let today = Local::now().date_naive();

    (0..42)
        .map(|offset| {
            let date = grid_start + Duration::days(offset);
            DateTimePickerDay {
                date,
                is_current_month: date.month() == selected_date.month(),
                is_selected: date == selected_date,
                is_today: date == today,
            }
        })
        .collect()
}

/// 渲染时间部分的加减控件，保留日历式选择器中常见的紧凑时间编辑区。
fn render_time_part_control(
    analysis_id: usize,
    input_kind: RuntimeFilterInputKind,
    part: RuntimeDateTimePart,
    value: String,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    div()
        .flex_1()
        .min_w(px(0.0))
        .flex()
        .items_center()
        .items_center()
        .gap_1()
        .child(render_time_step_button(
            format!("runtime-time-step-down-{analysis_id}-{input_kind:?}-{part:?}"),
            ArgusIcon::Minus,
            theme,
            cx.listener(move |app, _, _, cx| {
                cx.stop_propagation();
                app.adjust_runtime_filter_time(analysis_id, input_kind, part, -1, Some(cx));
                cx.notify();
            }),
        ))
        .child(
            div()
                .w(px(32.0))
                .h(px(24.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .border_1()
                .border_color(rgb(theme.border))
                .bg(rgb(theme.content))
                .child(
                    div()
                        .text_size(px(12.0))
                        .line_height(px(24.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(value),
                ),
        )
        .child(render_time_step_button(
            format!("runtime-time-step-up-{analysis_id}-{input_kind:?}-{part:?}"),
            ArgusIcon::Plus,
            theme,
            cx.listener(move |app, _, _, cx| {
                cx.stop_propagation();
                app.adjust_runtime_filter_time(analysis_id, input_kind, part, 1, Some(cx));
                cx.notify();
            }),
        ))
}

/// 渲染时间分隔符。
fn render_time_part_separator(theme: &AppTheme) -> impl IntoElement + use<> {
    div()
        .text_size(px(12.0))
        .line_height(px(24.0))
        .text_color(rgb(theme.foreground_muted))
        .child(":")
}

/// 渲染日期时间步进按钮。
fn render_time_step_button(
    id: String,
    icon: ArgusIcon,
    theme: &AppTheme,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(id))
        .size(px(22.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .cursor_pointer()
        .bg(rgb(theme.content))
        .hover(|this| this.bg(rgb(theme.selection)))
        .child(render_icon(icon, theme.foreground, 11.0))
        .on_click(on_click)
}

/// 渲染日期时间快捷动作按钮。
fn render_datetime_quick_button(
    analysis_id: usize,
    input_kind: RuntimeFilterInputKind,
    action: RuntimeDateTimeQuickAction,
    label: &'static str,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    div()
        .id(SharedString::from(format!(
            "runtime-time-quick-{analysis_id}-{input_kind:?}-{action:?}"
        )))
        .h(px(24.0))
        .px_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .cursor_pointer()
        .text_color(rgb(theme.info))
        .hover(|this| this.bg(rgb(theme.current_line)))
        .text_size(px(11.0))
        .line_height(px(24.0))
        .child(label)
        .on_click(cx.listener(move |app, _, _, cx| {
            cx.stop_propagation();
            app.apply_runtime_time_picker_quick_action(analysis_id, input_kind, action, Some(cx));
            cx.notify();
        }))
}

/// 渲染日期时间选择器确认按钮。
fn render_datetime_confirm_button(
    analysis_id: usize,
    label: &'static str,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> impl IntoElement + use<> {
    div()
        .id(SharedString::from(format!(
            "runtime-time-confirm-{analysis_id}"
        )))
        .h(px(24.0))
        .px_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .cursor_pointer()
        .text_color(rgb(theme.info))
        .hover(|this| this.bg(rgb(theme.current_line)))
        .text_size(px(11.0))
        .line_height(px(24.0))
        .child(label)
        .on_click(cx.listener(move |app, _, _, cx| {
            cx.stop_propagation();
            app.close_runtime_time_picker(analysis_id);
            cx.notify();
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证日历网格按周一开头补齐，固定 42 个单元格，避免月份切换时跳动。
    #[test]
    fn datetime_picker_days_start_on_monday_and_keep_six_weeks() {
        let days = datetime_picker_days(&DateTimePickerValue {
            year: 2026,
            month: 6,
            day: 25,
            hour: 0,
            minute: 0,
            second: 0,
        });

        assert_eq!(days.len(), 42);
        assert_eq!(
            days.first().map(|day| day.date),
            NaiveDate::from_ymd_opt(2026, 6, 1)
        );
        assert!(days.iter().any(|day| day.is_selected));
    }
}
