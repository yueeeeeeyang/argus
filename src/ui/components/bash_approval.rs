//! 文件职责：bash 命令审批确认卡片组件。
//! 创建日期：2026-09-14
//! 作者：Argus 开发团队
//! 主要功能：供智能分析窗口与交互助手面板共用的审批状态模型和内联确认卡片渲染。

use gpui::{App, ClickEvent, FontWeight, SharedString, Window, div, prelude::*, px, rgb};
use std::time::SystemTime;

use crate::fonts::ARGUS_UI_FONT_FAMILY;
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};

/// 一条 bash 审批卡片的生命周期状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BashApprovalStatus {
    /// 等待用户答复。
    Pending,
    /// 已批准执行。
    Approved,
    /// 已拒绝执行；携带界面展示的中文原因。
    Denied(String),
}

/// 会话内的 bash 审批卡片状态；模型经工具等待用户决定。
#[derive(Clone, Debug)]
pub(crate) struct BashApprovalCard {
    /// 审批请求标识。
    pub request_id: String,
    /// 待审批的完整命令。
    pub command: String,
    /// 卡片创建时间；用于与消息按时间顺序交错展示。
    pub created_at: SystemTime,
    /// 当前审批状态。
    pub status: BashApprovalStatus,
}

impl BashApprovalCard {
    /// 创建一条等待答复的审批卡片。
    pub(crate) fn pending(request_id: String, command: String) -> Self {
        Self {
            request_id,
            command,
            created_at: SystemTime::now(),
            status: BashApprovalStatus::Pending,
        }
    }
}

/// 把工具侧审批结论原因映射为界面展示的中文说明。
pub(crate) fn bash_approval_reason_label(reason: &str) -> String {
    let tail = reason.rsplit('|').next().unwrap_or(reason);
    match tail {
        "user_decision" => "用户未批准这条命令".to_string(),
        "approval_timeout" => "长时间未答复，已自动拒绝".to_string(),
        "session_cancelled" => "会话已取消，按拒绝处理".to_string(),
        "session_ended" => "会话已结束，按拒绝处理".to_string(),
        "session_closed" => "会话界面已关闭，按拒绝处理".to_string(),
        other => other.to_string(),
    }
}

/// 渲染一条 bash 审批确认卡片；等待中提供批准/拒绝按钮，结论后固化状态说明。
///
/// `on_decision` 在用户点击按钮时收到是否批准；调用方负责把答复送回后台会话。
pub(crate) fn render_bash_approval_card(
    request_id: &str,
    command: &str,
    status: &BashApprovalStatus,
    theme: &AppTheme,
    max_width: f32,
    on_decision: impl Fn(bool, &ClickEvent, &mut Window, &mut App) + 'static + Clone,
) -> impl IntoElement {
    let approve_decision = on_decision.clone();
    let deny_decision = on_decision;
    div()
        .id(SharedString::from(format!("bash-approval-{request_id}")))
        .w_full()
        .py_2()
        .flex()
        .justify_center()
        .child(
            div()
                .w_full()
                .max_w(px(max_width))
                .rounded_lg()
                .border_1()
                .border_color(rgb(theme.border))
                .bg(rgb(theme.content))
                .p_4()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(12.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(theme.warning))
                        .child(render_icon(ArgusIcon::Settings, theme.warning, 14.0))
                        .child("模型请求执行命令，需要你确认"),
                )
                .child(
                    div()
                        .w_full()
                        .rounded_md()
                        .bg(rgb(theme.current_line))
                        .p_3()
                        .text_size(px(12.0))
                        .font_family(ARGUS_UI_FONT_FAMILY)
                        .child(command.to_string()),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(11.0))
                        .text_color(rgb(theme.foreground_muted))
                        .child(
                            "命令不在只读白名单内或涉及工作目录外路径；批准后仅在日志工作目录内执行，超时未答复按拒绝处理。",
                        ),
                )
                .when(status == &BashApprovalStatus::Pending, |this| {
                    this.child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(approval_action_button(
                                "bash-approval-deny",
                                "拒绝",
                                false,
                                theme,
                                {
                                    let deny_decision = deny_decision.clone();
                                    move |event, window, app| {
                                        app.stop_propagation();
                                        deny_decision(false, event, window, app);
                                    }
                                },
                            ))
                            .child(approval_action_button(
                                "bash-approval-approve",
                                "批准执行",
                                true,
                                theme,
                                move |event, window, app| {
                                    app.stop_propagation();
                                    approve_decision(true, event, window, app);
                                },
                            )),
                    )
                })
                .when(status != &BashApprovalStatus::Pending, |this| match status {
                    BashApprovalStatus::Approved => this.child(
                        div()
                            .flex()
                            .justify_end()
                            .text_size(px(11.0))
                            .text_color(rgb(theme.info))
                            .child("已批准执行"),
                    ),
                    BashApprovalStatus::Denied(reason) => this.child(
                        div()
                            .flex()
                            .justify_end()
                            .text_size(px(11.0))
                            .text_color(rgb(theme.error))
                            .child(format!("已拒绝：{reason}")),
                    ),
                    BashApprovalStatus::Pending => this,
                }),
        )
}

/// 审批卡片底部动作按钮。
fn approval_action_button(
    id: &'static str,
    label: &'static str,
    primary: bool,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .h(px(28.0))
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(if primary {
            theme.selection
        } else {
            theme.current_line
        }))
        .text_size(px(11.0))
        .cursor_pointer()
        .hover(|hover| hover.opacity(0.82))
        .on_click(on_click)
        .child(label)
}
