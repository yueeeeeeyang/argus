//! 文件职责：渲染 AI 日志分析的独立轨迹与报告窗口。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-16
//! 作者：Argus 开发团队
//! 主要功能：流式展示模型思考、正文、工具轨迹与 Token 用量，并在底部悬浮文本域中接收会话追加提示。

use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, FocusHandle, FontWeight, IntoElement,
    KeyDownEvent, Pixels, Render, ScrollHandle, Subscription, Window, div, prelude::*, px, rgb,
};
use std::collections::HashSet;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use crate::agent::{
    AgentBudgetSnapshot, AgentEvent, AgentLogProfileMatchSummary, AgentSessionStatus,
    AgentStreamKind, AgentTraceEntry, AgentTraceKind, AgentUserMessage, AgentUserMessageStatus,
    DiagnosticReport, SourceScopeSnapshot,
};
use crate::app::{ArgusApp, TextInputState, observe_app_theme};
use crate::config::{LogNameMatcherMode, LogNameMatcherTarget};
use crate::fonts::ARGUS_UI_FONT_FAMILY;
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_round_icon_button};
use crate::ui::components::input::{
    InputAccessory, InputPointerAction, InputPointerEvent, NativeInput, Textarea,
    TextareaAccessoryPosition, TextareaScrollState, TextareaStyle, render_textarea,
};
use crate::ui::components::input_behavior::{LocalInputAction, handle_local_input_key};
use crate::ui::components::loading_spinner::render_loading_spinner;
use crate::ui::components::window_title_bar::render_window_title_bar;

/// 单条会话追加提示 UTF-8 字节上限。
const AGENT_MESSAGE_MAX_BYTES: usize = 4 * 1024;
/// 单会话最多接受的追加提示条数。
const AGENT_MESSAGE_MAX_COUNT: usize = 20;
/// 单会话追加提示累计 UTF-8 字节上限。
const AGENT_MESSAGE_TOTAL_MAX_BYTES: usize = 32 * 1024;
/// 窗口中保留的最大轻量轨迹条目数量。
const AGENT_TRACE_MAX_COUNT: usize = 1000;
/// 消息瀑布流和底部输入区的最大阅读宽度。
const AGENT_STREAM_MAX_WIDTH: f32 = 860.0;
/// 消息瀑布流纵向滚动条宽度；轨道由 GPUI 保持透明，仅显示滑块。
const AGENT_STREAM_SCROLLBAR_WIDTH: f32 = 8.0;
/// 用户向下滚动到距底部该阈值内时恢复自动跟随，避免像素取整导致按钮无法消失。
const AGENT_STREAM_BOTTOM_THRESHOLD: f32 = 24.0;

/// Agent 独立窗口根视图。
pub(crate) struct AgentWindow {
    /// 主窗口应用实体，用于证据导航和主题同步。
    app: Entity<ArgusApp>,
    /// 当前主题快照。
    theme: AppTheme,
    /// 会话随机 ID。
    session_id: String,
    /// 用户初始问题。
    question: String,
    /// 当前状态机状态。
    status: AgentSessionStatus,
    /// 本次会话所选模型的上下文窗口 Token 数。
    context_window_tokens: u64,
    /// 增量轻量轨迹，不保存完整工具输出或日志原文。
    traces: Vec<AgentTraceEntry>,
    /// 用户主动展开的连续工具轨迹组，键由组首条轨迹时间生成。
    expanded_tool_groups: HashSet<i64>,
    /// 分析消息流滚动句柄，用于用户停留最新位置时自动跟随新事件。
    trace_scroll: ScrollHandle,
    /// 是否自动跟随消息流最新位置；用户主动上滚后关闭，回到底部后恢复。
    is_trace_following: bool,
    /// 最新资源预算快照。
    budget: AgentBudgetSnapshot,
    /// 最终结构化报告。
    report: Option<DiagnosticReport>,
    /// 报告持久化路径，仅供界面提示。
    report_path: Option<String>,
    /// 底部追加提示输入状态。
    message_input: TextInputState,
    /// 提示输入框滚动句柄。
    message_scroll: ScrollHandle,
    /// 提示文本域自绘滚动状态。
    message_scroll_state: TextareaScrollState,
    /// 已提交提示及其消费状态。
    user_messages: Vec<AgentUserMessage>,
    /// 发送给后台编排器的提示队列。
    user_message_sender: async_channel::Sender<AgentUserMessage>,
    /// 会话取消令牌。
    cancellation: tokio_util::sync::CancellationToken,
    /// 与编排器共享的未消费提示计数器。
    pending_user_messages: Arc<AtomicUsize>,
    /// 提示入队和报告阶段关闭入口共用的线性化门闩。
    user_message_gate: Arc<std::sync::Mutex<bool>>,
    /// 会话来源快照，用于把报告中的不透明引用安全解析为内部来源 ID。
    scope: Arc<SourceScopeSnapshot>,
    /// 提示输入框焦点句柄。
    message_focus: FocusHandle,
    /// 是否已完成首次聚焦。
    has_focused: bool,
    /// 是否已经注册系统窗口关闭拦截。
    has_registered_close_guard: bool,
    /// 运行中关闭时显示的确认浮层。
    show_close_confirmation: bool,
    /// 最近一次用户输入错误。
    input_error: Option<String>,
    /// 主题观察订阅。
    _theme_observer: Subscription,
}

impl AgentWindow {
    /// 创建 Agent 独立窗口并启动后台事件轮询。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        app: Entity<ArgusApp>,
        theme: AppTheme,
        session_id: String,
        question: String,
        user_message_sender: async_channel::Sender<AgentUserMessage>,
        event_receiver: async_channel::Receiver<AgentEvent>,
        cancellation: tokio_util::sync::CancellationToken,
        pending_user_messages: Arc<AtomicUsize>,
        user_message_gate: Arc<std::sync::Mutex<bool>>,
        scope: Arc<SourceScopeSnapshot>,
        match_summaries: Vec<AgentLogProfileMatchSummary>,
        context_window_tokens: u64,
        cx: &mut Context<Self>,
    ) -> Self {
        let _theme_observer = observe_app_theme(cx, &app, theme.clone(), |view, next_theme, _| {
            view.theme = next_theme.clone();
        });
        cx.spawn(async move |view, cx| {
            while let Ok(first_event) = event_receiver.recv().await {
                let mut events = Vec::with_capacity(32);
                events.push(first_event);
                // 一次刷新批量吸收已经到达的流式增量，降低高频 Token 事件触发的重复布局开销。
                while events.len() < 128 {
                    let Ok(event) = event_receiver.try_recv() else {
                        break;
                    };
                    events.push(event);
                }
                if view
                    .update(cx, |window, cx| {
                        for event in events {
                            window.apply_event(event);
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let message_input = TextInputState {
            is_focused: true,
            ..TextInputState::default()
        };
        let source_scan_summary = format_source_scan_summary(&scope, &match_summaries);
        Self {
            app,
            theme,
            session_id,
            question,
            status: AgentSessionStatus::Created,
            context_window_tokens,
            traces: vec![AgentTraceEntry::new(
                AgentTraceKind::Status,
                "会话已创建",
                source_scan_summary,
            )],
            expanded_tool_groups: HashSet::new(),
            trace_scroll: ScrollHandle::new(),
            is_trace_following: true,
            budget: AgentBudgetSnapshot::default(),
            report: None,
            report_path: None,
            message_input,
            message_scroll: ScrollHandle::new(),
            message_scroll_state: TextareaScrollState::new(),
            user_messages: Vec::new(),
            user_message_sender,
            cancellation,
            pending_user_messages,
            user_message_gate,
            scope,
            message_focus: cx.focus_handle(),
            has_focused: false,
            has_registered_close_guard: false,
            show_close_confirmation: false,
            input_error: None,
            _theme_observer,
        }
    }

    /// 应用一个后台事件并维护有限内存轨迹。
    fn apply_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Status(status) => {
                self.status = status;
                self.push_trace(AgentTraceEntry::new(
                    AgentTraceKind::Status,
                    format!("状态：{}", status.label()),
                    "会话状态已更新",
                ));
            }
            AgentEvent::Trace(trace) => self.push_trace(trace),
            AgentEvent::Budget(budget) => self.budget = budget,
            AgentEvent::StreamDelta(kind, delta) => self.apply_stream_delta(kind, delta),
            AgentEvent::UserMessageConsumed(message_id) => {
                if let Some(message) = self
                    .user_messages
                    .iter_mut()
                    .find(|message| message.message_id == message_id)
                {
                    message.status = AgentUserMessageStatus::Consumed;
                }
                self.push_trace(AgentTraceEntry::new(
                    AgentTraceKind::User,
                    "追加提示已消费",
                    "提示已串行注入下一次模型请求",
                ));
            }
            AgentEvent::UserMessageRejected(message_id, reason) => {
                if let Some(message) = self
                    .user_messages
                    .iter_mut()
                    .find(|message| message.message_id == message_id)
                {
                    message.status = AgentUserMessageStatus::Rejected;
                }
                self.push_trace(AgentTraceEntry::new(
                    AgentTraceKind::Warning,
                    "追加提示未发送",
                    reason,
                ));
            }
            AgentEvent::Report(report, report_path) => {
                self.push_trace(AgentTraceEntry::new(
                    AgentTraceKind::Report,
                    "诊断报告已生成",
                    report.summary.clone(),
                ));
                self.report = Some(report);
                self.report_path = report_path;
            }
            AgentEvent::Failed(message) => {
                self.input_error = Some(message.clone());
                self.push_trace(AgentTraceEntry::new(
                    AgentTraceKind::Warning,
                    "分析失败",
                    message,
                ));
            }
        }
    }

    /// 合并相邻同类模型增量，让每轮思考和正文各自形成持续增长的一条瀑布流消息。
    fn apply_stream_delta(&mut self, kind: AgentStreamKind, delta: String) {
        if delta.is_empty() {
            return;
        }
        let (trace_kind, title) = match kind {
            AgentStreamKind::Reasoning => (AgentTraceKind::Reasoning, "思考过程"),
            AgentStreamKind::Output => (AgentTraceKind::Output, "AI 输出"),
        };
        if let Some(last_trace) = self.traces.last_mut()
            && last_trace.kind == trace_kind
        {
            last_trace.detail.push_str(&delta);
            if self.is_trace_following {
                self.trace_scroll.scroll_to_bottom();
            }
            return;
        }
        self.push_trace(AgentTraceEntry::new(trace_kind, title, delta));
    }

    /// 追加轨迹并丢弃最旧的超限条目。
    fn push_trace(&mut self, trace: AgentTraceEntry) {
        self.traces.push(trace);
        if self.traces.len() > AGENT_TRACE_MAX_COUNT {
            let drain_count = self.traces.len() - AGENT_TRACE_MAX_COUNT;
            self.traces.drain(0..drain_count);
        }
        if self.is_trace_following {
            self.trace_scroll.scroll_to_bottom();
        }
    }

    /// 根据瀑布流滚轮方向和预计位置更新自动跟随状态。
    fn handle_trace_scroll(&mut self, delta_y: Pixels) {
        self.is_trace_following = trace_following_after_wheel(
            self.is_trace_following,
            self.trace_scroll.offset().y,
            self.trace_scroll.max_offset().height,
            delta_y,
        );
    }

    /// 跳转到消息流底部并恢复后续流式事件自动跟随。
    fn jump_trace_to_latest(&mut self) {
        self.is_trace_following = true;
        self.trace_scroll.scroll_to_bottom();
    }

    /// 提交底部提示；消息只入队，不创建并发模型请求。
    fn submit_message(&mut self) {
        let content = self.message_input.value.trim().to_string();
        if content.is_empty() {
            self.input_error = Some("请输入补充提示".to_string());
            return;
        }
        if self.status.is_terminal() || self.status == AgentSessionStatus::Cancelling {
            self.input_error = Some("当前会话已经结束，不能继续发送提示".to_string());
            return;
        }
        if content.len() > AGENT_MESSAGE_MAX_BYTES {
            self.input_error = Some("单条提示不能超过 4 KiB".to_string());
            return;
        }
        if self.user_messages.len() >= AGENT_MESSAGE_MAX_COUNT {
            self.input_error = Some("当前会话已达到 20 条追加提示上限".to_string());
            return;
        }
        let current_bytes: usize = self
            .user_messages
            .iter()
            .map(|message| message.content.len())
            .sum();
        if current_bytes.saturating_add(content.len()) > AGENT_MESSAGE_TOTAL_MAX_BYTES {
            self.input_error = Some("当前会话追加提示已达到累计 32 KiB 上限".to_string());
            return;
        }
        let message = AgentUserMessage::queued(content.clone());
        // 校验入口状态和写入有界队列必须持有同一门闩，避免报告提交竞态静默遗漏消息。
        let send_result = self
            .user_message_gate
            .lock()
            .map_err(|_| "Agent 提示入口状态已损坏".to_string())
            .and_then(|accepting| {
                if !*accepting {
                    return Err("Agent 已进入报告或终止阶段，不能继续发送提示".to_string());
                }
                self.pending_user_messages.fetch_add(1, Ordering::AcqRel);
                self.user_message_sender
                    .try_send(message.clone())
                    .map_err(|_| {
                        self.pending_user_messages.fetch_sub(1, Ordering::AcqRel);
                        "Agent 提示队列已经关闭".to_string()
                    })
            });
        if let Err(message) = send_result {
            self.input_error = Some(message);
            return;
        }
        self.user_messages.push(message);
        self.message_input = TextInputState::default();
        self.message_input.is_focused = true;
        self.input_error = None;
        self.push_trace(AgentTraceEntry::new(
            AgentTraceKind::User,
            "用户追加提示（排队中）",
            content,
        ));
    }

    /// 处理底部多行提示输入按键。
    fn handle_message_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match handle_local_input_key(&mut self.message_input, &event.keystroke, true, cx) {
            LocalInputAction::Submit => self.submit_message(),
            LocalInputAction::Changed => self.input_error = None,
            LocalInputAction::Close => self.message_input.clear_focus(),
            LocalInputAction::None => {}
        }
    }

    /// 请求取消会话，实际终态由后台任务确认后发布。
    fn cancel_session(&mut self) {
        if self.status.is_terminal() || self.status == AgentSessionStatus::Cancelling {
            return;
        }
        self.status = AgentSessionStatus::Cancelling;
        self.cancellation.cancel();
        self.push_trace(AgentTraceEntry::new(
            AgentTraceKind::Status,
            "正在取消",
            "已通知模型和日志工具在最近边界停止",
        ));
    }

    /// 处理自定义关闭按钮；运行态先显示确认浮层。
    fn request_close(&mut self, window: &mut Window) {
        if self.status.is_terminal() {
            window.remove_window();
        } else {
            self.show_close_confirmation = true;
        }
    }

    /// 切换一组连续工具轨迹的展开状态。
    fn toggle_tool_group(&mut self, group_id: i64) {
        if !self.expanded_tool_groups.remove(&group_id) {
            self.expanded_tool_groups.insert(group_id);
        }
    }
}

/// 格式化完整来源扫描和逐规则命中统计，供会话首条状态消息展示。
fn format_source_scan_summary(
    scope: &SourceScopeSnapshot,
    match_summaries: &[AgentLogProfileMatchSummary],
) -> String {
    let mut lines = vec![format!(
        "来源树已完整扫描，共发现 {} 个日志文件，最终匹配 {} 种日志类型说明。",
        scope.sources.len(),
        scope.profiles.len()
    )];
    if match_summaries.is_empty() {
        lines.push("当前没有已启用的日志类型匹配规则。".to_string());
        return lines.join("\n");
    }

    lines.push("规则命中统计（同一文件可以命中多条规则）：".to_string());
    for summary in match_summaries {
        lines.push(format!(
            "{}（优先级 {}）：规则命中 {} 个文件，最终采用 {} 个文件",
            summary.profile_name,
            summary.priority,
            summary.matched_file_count,
            summary.selected_file_count
        ));
        for (rule_index, rule) in summary.rules.iter().enumerate() {
            lines.push(format!(
                "  规则 {} · {} · {} · {} · “{}”：命中 {} 个文件",
                rule_index + 1,
                matcher_target_label(rule.target),
                matcher_mode_label(rule.mode),
                if rule.case_sensitive {
                    "区分大小写"
                } else {
                    "忽略大小写"
                },
                compact_matcher_pattern(&rule.pattern),
                rule.matched_file_count
            ));
        }
    }
    lines.join("\n")
}

/// 返回规则目标字段的中文显示名称。
fn matcher_target_label(target: LogNameMatcherTarget) -> &'static str {
    match target {
        LogNameMatcherTarget::FileName => "文件名",
        LogNameMatcherTarget::RelativePath => "相对路径",
    }
}

/// 返回规则匹配算法的中文显示名称。
fn matcher_mode_label(mode: LogNameMatcherMode) -> &'static str {
    match mode {
        LogNameMatcherMode::Exact => "完全相等",
        LogNameMatcherMode::Prefix => "前缀",
        LogNameMatcherMode::Suffix => "后缀",
        LogNameMatcherMode::Contains => "包含",
        LogNameMatcherMode::Regex => "正则",
    }
}

/// 裁剪并转义规则模式中的换行和制表符，防止配置内容破坏状态列表布局。
fn compact_matcher_pattern(pattern: &str) -> String {
    const MAX_DISPLAY_CHARS: usize = 120;
    let escaped = pattern
        .replace('\r', "\\r")
        .replace('\n', "\\n")
        .replace('\t', "\\t");
    let mut chars = escaped.chars();
    let compact = chars.by_ref().take(MAX_DISPLAY_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{compact}…")
    } else {
        compact
    }
}

impl Drop for AgentWindow {
    /// 窗口被系统或应用销毁时取消仍在运行的会话，禁止形成不可见后台 Agent。
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Ok(mut accepting) = self.user_message_gate.lock() {
            *accepting = false;
        }
    }
}

impl Render for AgentWindow {
    /// 渲染轨迹、预算、报告和底部悬浮对话框。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.has_registered_close_guard {
            let entity = cx.entity();
            window.on_window_should_close(cx, move |_, app_cx| {
                entity.update(app_cx, |view, cx| {
                    if view.status.is_terminal() {
                        true
                    } else {
                        view.show_close_confirmation = true;
                        cx.notify();
                        false
                    }
                })
            });
            self.has_registered_close_guard = true;
        }
        if !self.has_focused {
            self.message_focus.focus(window);
            self.has_focused = true;
        }
        let entity = cx.entity();
        let native_entity = entity.clone();
        let native_input = NativeInput::new(self.message_focus.clone(), move |edit, _, app_cx| {
            native_entity.update(app_cx, |view, cx| {
                view.message_input.apply_native_edit(&edit);
                view.input_error = None;
                cx.notify();
            });
        });
        let close_entity = entity.clone();
        let cancel_entity = entity.clone();
        let key_entity = entity.clone();
        let click_entity = entity.clone();
        let pointer_entity = entity.clone();
        let send_entity = entity.clone();
        let reject_close_entity = entity.clone();
        let confirm_close_entity = entity.clone();
        let theme = self.theme.clone();
        let status = self.status;
        let can_send_message = !status.is_terminal()
            && status != AgentSessionStatus::Cancelling
            && !self.message_input.value.trim().is_empty();
        // 跟随态在每次布局前重新声明滚到底部，既覆盖新增消息，也覆盖流式正文导致的原行增高。
        if self.is_trace_following {
            self.trace_scroll.scroll_to_bottom();
        }
        let show_jump_to_latest = !self.is_trace_following;
        let jump_to_latest_entity = entity.clone();

        div()
            .id("agent-window-root")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(rgb(theme.background))
            .font_family(ARGUS_UI_FONT_FAMILY)
            .text_color(rgb(theme.foreground))
            .child(render_window_title_bar(
                "agent-window-close",
                "关闭智能分析",
                46.0,
                true,
                &theme,
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(render_icon(ArgusIcon::SmartAnalysis, theme.info, 17.0))
                    .child(div().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).child("AI 日志分析"))
                    .child(div().text_size(px(11.0)).text_color(rgb(theme.foreground_muted)).child(format!("{} · {}", status.label(), short_id(&self.session_id)))),
                move |_, window, app_cx| {
                    close_entity.update(app_cx, |view, cx| {
                        view.request_close(window);
                        cx.notify();
                    });
                },
            ))
            .child(render_budget_bar(
                self.budget,
                self.context_window_tokens,
                status,
                &theme,
                cancel_entity,
            ))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_hidden()
                    .child(render_message_stream(
                        &self.question,
                        &self.traces,
                        status,
                        self.report.as_ref(),
                        self.report_path.as_deref(),
                        self.app.clone(),
                        self.scope.clone(),
                        self.trace_scroll.clone(),
                        &self.expanded_tool_groups,
                        entity.clone(),
                        &theme,
                    )),
            )
            .child(div().h(px(148.0)).flex_none())
            .when(show_jump_to_latest, |this| {
                this.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom(px(158.0))
                        .px_5()
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .w_full()
                                .max_w(px(AGENT_STREAM_MAX_WIDTH))
                                .flex()
                                .justify_center()
                                .child(
                                    div()
                                        .p_1()
                                        .rounded_full()
                                        .border_1()
                                        .border_color(rgb(theme.border))
                                        .bg(rgb(theme.content))
                                        .shadow_lg()
                                        .child(render_round_icon_button(
                                            "agent-jump-to-latest",
                                            ArgusIcon::ArrowDown,
                                            "跳转到最新消息",
                                            false,
                                            IconButtonSize::Small,
                                            &theme,
                                            move |_, _, app_cx| {
                                                app_cx.stop_propagation();
                                                jump_to_latest_entity.update(
                                                    app_cx,
                                                    |view, cx| {
                                                        view.jump_trace_to_latest();
                                                        cx.notify();
                                                    },
                                                );
                                            },
                                        )),
                                ),
                        ),
                )
            })
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom(px(18.0))
                    .px_5()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(AGENT_STREAM_MAX_WIDTH))
                            .child(render_textarea(
                        Textarea {
                            id: "agent-window-message-input",
                            placeholder: "在分析过程中补充线索或纠正方向（Cmd/Ctrl+Enter 发送）",
                            value: self.message_input.value.clone(),
                            is_disabled: status.is_terminal() || status == AgentSessionStatus::Cancelling,
                            is_focused: self.message_input.is_focused,
                            cursor_index: self.message_input.cursor,
                            selection_range: self.message_input.selection_range(),
                            marked_range: self.message_input.marked_range.clone(),
                            is_pointer_selecting: self.message_input.selection_drag.is_some(),
                            visible_lines: 4,
                            fill_height: false,
                            scroll_handle: self.message_scroll.clone(),
                            scroll_state: self.message_scroll_state.clone(),
                            style: TextareaStyle::Composer,
                            trailing_accessory: Some(InputAccessory {
                                id: "agent-message-send",
                                icon: ArgusIcon::ArrowUp,
                                tooltip: "发送提示",
                            }),
                            trailing_accessory_position: TextareaAccessoryPosition::BottomRight,
                            trailing_accessory_always_visible: true,
                            trailing_accessory_selected: can_send_message,
                            native_input: Some(native_input),
                        },
                        &theme,
                        move |event, _, app_cx| {
                            app_cx.stop_propagation();
                            key_entity.update(app_cx, |view, cx| {
                                view.handle_message_key(event, cx);
                                cx.notify();
                            });
                        },
                        move |_, window, app_cx| {
                            app_cx.stop_propagation();
                            click_entity.update(app_cx, |view, cx| {
                                view.message_input.is_focused = true;
                                view.message_focus.focus(window);
                                cx.notify();
                            });
                        },
                        move |event: &InputPointerEvent, _, app_cx| {
                            pointer_entity.update(app_cx, |view, cx| {
                                match event.action {
                                    InputPointerAction::Begin => view.message_input.begin_pointer_selection(event.character_index, event.granularity),
                                    InputPointerAction::Extend => view.message_input.update_pointer_selection(event.character_index),
                                    InputPointerAction::Finish => view.message_input.finish_pointer_selection(),
                                }
                                cx.notify();
                            });
                        },
                        move |_, _, app_cx| {
                            app_cx.stop_propagation();
                            if can_send_message {
                                send_entity.update(app_cx, |view, cx| {
                                    view.submit_message();
                                    cx.notify();
                                });
                            }
                        },
                            ))
                            .child(
                                div()
                                    .mt_2()
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .child(
                                        div()
                                            .text_size(px(11.0))
                                            .text_color(rgb(if self.input_error.is_some() { theme.error } else { theme.foreground_muted }))
                                            .child(self.input_error.clone().unwrap_or_else(|| "消息将在当前模型或工具调用结束后串行消费".to_string())),
                                    ),
                            ),
                    ),
            )
            .when(self.show_close_confirmation, |this| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(gpui::rgba(0x000000aa))
                        .child(
                            div()
                                .w(px(420.0))
                                .p_5()
                                .rounded_lg()
                                .border_1()
                                .border_color(rgb(theme.border))
                                .bg(rgb(theme.content))
                                .child(div().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).child("取消分析并关闭窗口？"))
                                .child(div().mt_2().text_size(px(12.0)).text_color(rgb(theme.foreground_muted)).child("关闭后不会留下不可见的后台 Agent；当前任务会先收到取消信号。"))
                                .child(
                                    div()
                                        .mt_4()
                                        .flex()
                                        .justify_end()
                                        .gap_2()
                                        .child(action_button("agent-close-keep", "继续分析", false, true, &theme, move |_, _, app_cx| {
                                            reject_close_entity.update(app_cx, |view, cx| {
                                                view.show_close_confirmation = false;
                                                cx.notify();
                                            });
                                        }))
                                        .child(action_button("agent-close-confirm", "取消并关闭", true, true, &theme, move |_, window, app_cx| {
                                            confirm_close_entity.update(app_cx, |view, _| view.cancel_session());
                                            window.remove_window();
                                        })),
                                ),
                        ),
                )
            })
    }
}

/// 渲染资源预算条。
fn render_budget_bar(
    budget: AgentBudgetSnapshot,
    context_window_tokens: u64,
    status: AgentSessionStatus,
    theme: &AppTheme,
    cancel_entity: Entity<AgentWindow>,
) -> impl IntoElement + use<> {
    let total_calls = budget.model_requests.saturating_add(budget.tool_calls);
    let (context_title, context_detail) = format_context_metric(budget, context_window_tokens);
    div()
        .h(px(56.0))
        .px_4()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.side_bar))
        .child(div().w(px(104.0)).flex_none().child(render_budget_metric(
            format!("调用 {total_calls}"),
            format!(
                "模型 {} · 工具 {}",
                budget.model_requests, budget.tool_calls
            ),
            theme,
        )))
        .child(render_budget_divider(theme))
        .child(div().flex_1().min_w(px(150.0)).child(render_budget_metric(
            format!("Token {}", format_compact_tokens(budget.total_tokens)),
            format_token_breakdown(budget),
            theme,
        )))
        .child(render_budget_divider(theme))
        .child(div().w(px(148.0)).flex_none().child(render_budget_metric(
            context_title,
            context_detail,
            theme,
        )))
        .child(render_budget_divider(theme))
        .child(div().w(px(188.0)).flex_none().child(render_budget_metric(
            "数据读取".to_string(),
            format!(
                "扫描 {} · 原文 {}/512 KiB",
                format_bytes(budget.local_scan_bytes),
                format_bytes(budget.raw_log_bytes)
            ),
            theme,
        )))
        .child(render_budget_divider(theme))
        .child(div().w(px(78.0)).flex_none().child(render_budget_metric(
            format!("耗时 {}s", budget.elapsed_seconds),
            status.label().to_string(),
            theme,
        )))
        .when(
            !status.is_terminal() && status != AgentSessionStatus::Cancelling,
            |this| {
                this.child(div().ml_2().flex_none().child(action_button(
                    "agent-cancel",
                    "取消",
                    false,
                    true,
                    theme,
                    move |_, _, app_cx| {
                        cancel_entity.update(app_cx, |view, cx| {
                            view.cancel_session();
                            cx.notify();
                        });
                    },
                )))
            },
        )
}

/// 渲染状态栏中的两行指标，首行突出总量或比例，次行补充组成信息。
fn render_budget_metric(
    title: String,
    detail: String,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let foreground = theme.foreground;
    let foreground_muted = theme.foreground_muted;
    div()
        .min_w(px(0.0))
        .px_2()
        .overflow_hidden()
        .child(
            div()
                .whitespace_nowrap()
                .text_size(px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(foreground))
                .child(title),
        )
        .child(
            div()
                .mt(px(2.0))
                .whitespace_nowrap()
                .text_size(px(10.0))
                .text_color(rgb(foreground_muted))
                .child(detail),
        )
}

/// 渲染状态栏指标间的轻量纵向分隔线，替代连续点号造成的视觉混杂。
fn render_budget_divider(theme: &AppTheme) -> impl IntoElement + use<> {
    let border = theme.border;
    div().w(px(1.0)).h(px(30.0)).flex_none().bg(rgb(border))
}

/// 渲染单列消息瀑布流，并把最终报告作为同一消息流中的最后一条 Agent 消息。
#[allow(clippy::too_many_arguments)]
fn render_message_stream(
    question: &str,
    traces: &[AgentTraceEntry],
    status: AgentSessionStatus,
    report: Option<&DiagnosticReport>,
    report_path: Option<&str>,
    app: Entity<ArgusApp>,
    scope: Arc<SourceScopeSnapshot>,
    scroll_handle: ScrollHandle,
    expanded_tool_groups: &HashSet<i64>,
    agent_window: Entity<AgentWindow>,
    theme: &AppTheme,
) -> impl IntoElement {
    let scroll_event_entity = agent_window.clone();
    // 仅让最后一条仍在执行的系统、模型、思考、输出或工具消息旋转，历史消息保持静态图标。
    let active_trace_index = if status.is_terminal() {
        None
    } else {
        traces.iter().rposition(|trace| {
            matches!(
                trace.kind,
                AgentTraceKind::Status
                    | AgentTraceKind::Model
                    | AgentTraceKind::Reasoning
                    | AgentTraceKind::Output
                    | AgentTraceKind::Tool
            )
        })
    };
    let trace_elements = render_trace_stream_items(
        traces,
        report.is_some(),
        active_trace_index,
        expanded_tool_groups,
        agent_window,
        theme,
    );
    let mut stream = div()
        .id("agent-message-stream")
        .flex_1()
        .h_full()
        .min_w(px(0.0))
        .overflow_y_scroll()
        .scrollbar_width(px(AGENT_STREAM_SCROLLBAR_WIDTH))
        .track_scroll(&scroll_handle)
        .on_scroll_wheel(move |event, window, app_cx| {
            let delta_y = event.delta.pixel_delta(window.line_height()).y;
            scroll_event_entity.update(app_cx, |view, cx| {
                view.handle_trace_scroll(delta_y);
                cx.notify();
            });
        })
        .child(
            div()
                .w_full()
                .px_6()
                .pt_6()
                .pb_3()
                .flex()
                .justify_center()
                .child(
                    div()
                        .w_full()
                        .max_w(px(AGENT_STREAM_MAX_WIDTH))
                        .flex()
                        .gap_3()
                        .child(
                            div()
                                .w(px(20.0))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(render_icon(ArgusIcon::ArrowRight, theme.info, 14.0)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("你"),
                                )
                                .child(
                                    div()
                                        .mt_1()
                                        .text_size(px(13.0))
                                        .line_height(px(20.0))
                                        .child(question.to_string()),
                                ),
                        ),
                ),
        )
        .children(trace_elements);
    if let Some(report) = report {
        stream = stream.child(
            div()
                .w_full()
                .px_6()
                .pt_3()
                .pb_8()
                .flex()
                .justify_center()
                .child(render_report_message(
                    report,
                    report_path,
                    app,
                    scope,
                    theme,
                )),
        );
    } else {
        stream = stream.child(div().h(px(28.0)).flex_none());
    }
    stream
}

/// 把普通轨迹逐条渲染，并把相邻工具轨迹折叠为一个可展开的消息行。
fn render_trace_stream_items(
    traces: &[AgentTraceEntry],
    has_report: bool,
    active_trace_index: Option<usize>,
    expanded_tool_groups: &HashSet<i64>,
    agent_window: Entity<AgentWindow>,
    theme: &AppTheme,
) -> Vec<AnyElement> {
    let mut elements = Vec::new();
    let mut index = 0;
    while index < traces.len() {
        let trace = &traces[index];
        // 完整报告会在流末尾展开，避免再次显示仅含摘要的报告事件。
        if trace.kind == AgentTraceKind::Report && has_report {
            index += 1;
            continue;
        }
        if trace.kind != AgentTraceKind::Tool {
            elements.push(
                render_trace_message(trace, index, active_trace_index == Some(index), theme)
                    .into_any_element(),
            );
            index += 1;
            continue;
        }

        let group_start = index;
        while index < traces.len() && traces[index].kind == AgentTraceKind::Tool {
            index += 1;
        }
        let group = &traces[group_start..index];
        let group_id = tool_group_id(&group[0]);
        let is_active = active_trace_index
            .is_some_and(|active_index| (group_start..index).contains(&active_index));
        elements.push(
            render_tool_group_message(
                group,
                group_start,
                group_id,
                expanded_tool_groups.contains(&group_id),
                is_active,
                agent_window.clone(),
                theme,
            )
            .into_any_element(),
        );
    }
    elements
}

/// 使用组首条轨迹的纳秒时间建立会话内稳定展开键。
fn tool_group_id(trace: &AgentTraceEntry) -> i64 {
    trace
        .created_at
        .timestamp_nanos_opt()
        .unwrap_or_else(|| trace.created_at.timestamp_micros())
}

/// 渲染一组连续工具轨迹；折叠时只展示最后一条，展开后按原顺序展示全部明细。
fn render_tool_group_message(
    traces: &[AgentTraceEntry],
    trace_index: usize,
    group_id: i64,
    is_expanded: bool,
    is_active: bool,
    agent_window: Entity<AgentWindow>,
    theme: &AppTheme,
) -> impl IntoElement {
    let last_trace = traces.last().expect("工具轨迹组不能为空");
    let leading = if is_active {
        render_loading_spinner(
            ("agent-tool-group-loading", trace_index),
            trace_color(AgentTraceKind::Tool, theme),
            14.0,
        )
    } else {
        render_icon(
            trace_icon(AgentTraceKind::Tool),
            trace_color(AgentTraceKind::Tool, theme),
            14.0,
        )
        .into_any_element()
    };
    let title = if is_expanded {
        format!("连续工具轨迹 · {} 条", traces.len())
    } else {
        last_trace.title.clone()
    };
    let detail = if is_expanded {
        "已展开全部工具调用和结果".to_string()
    } else {
        last_trace.detail.clone()
    };
    let detail_elements = if is_expanded {
        traces
            .iter()
            .map(|trace| {
                div()
                    .ml(px(30.0))
                    .pl_3()
                    .py_2()
                    .border_l_1()
                    .border_color(rgb(theme.border))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(trace.title.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(10.0))
                                    .text_color(rgb(theme.foreground_muted))
                                    .opacity(0.55)
                                    .child(trace.created_at.format("%H:%M:%S").to_string()),
                            ),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_size(px(11.0))
                            .line_height(px(17.0))
                            .text_color(rgb(theme.foreground_muted))
                            .child(trace.detail.clone()),
                    )
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    div().w_full().px_6().py_2().flex().justify_center().child(
        div()
            .id(("agent-tool-group", trace_index))
            .w_full()
            .max_w(px(AGENT_STREAM_MAX_WIDTH))
            .py_1()
            .cursor_pointer()
            .hover(|this| this.bg(rgb(theme.current_line)))
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(
                        div()
                            .w(px(20.0))
                            .pt(px(2.0))
                            .flex_none()
                            .flex()
                            .justify_center()
                            .child(leading),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_size(px(12.0))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("工具"),
                                    )
                                    .child(
                                        div()
                                            .min_w(px(0.0))
                                            .text_size(px(12.0))
                                            .text_color(rgb(theme.foreground_muted))
                                            .child(title),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_size(px(10.0))
                                            .text_color(rgb(theme.foreground_muted))
                                            .opacity(0.55)
                                            .child(
                                                last_trace
                                                    .created_at
                                                    .format("%H:%M:%S")
                                                    .to_string(),
                                            ),
                                    )
                                    .child(render_icon(
                                        if is_expanded {
                                            ArgusIcon::Collapse
                                        } else {
                                            ArgusIcon::Expand
                                        },
                                        theme.foreground_muted,
                                        14.0,
                                    )),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_size(px(12.0))
                                    .line_height(px(19.0))
                                    .text_color(rgb(theme.foreground_muted))
                                    .child(detail),
                            ),
                    ),
            )
            .children(detail_elements)
            .on_click(move |_, _, app_cx| {
                app_cx.stop_propagation();
                agent_window.update(app_cx, |window, cx| {
                    window.toggle_tool_group(group_id);
                    cx.notify();
                });
            }),
    )
}

/// 把一条状态、模型、工具或用户事件渲染为无卡片边框的连续消息行。
fn render_trace_message(
    trace: &AgentTraceEntry,
    trace_index: usize,
    is_active: bool,
    theme: &AppTheme,
) -> impl IntoElement {
    let leading = if is_active {
        render_loading_spinner(
            ("agent-stream-loading", trace_index),
            trace_color(trace.kind, theme),
            14.0,
        )
    } else {
        render_icon(trace_icon(trace.kind), trace_color(trace.kind, theme), 14.0).into_any_element()
    };
    div().w_full().px_6().py_3().flex().justify_center().child(
        div()
            .w_full()
            .max_w(px(AGENT_STREAM_MAX_WIDTH))
            .flex()
            .gap_3()
            .child(
                div()
                    .w(px(20.0))
                    .pt(px(2.0))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .child(leading),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(trace_actor_label(trace.kind)),
                            )
                            .child(
                                div()
                                    .min_w(px(0.0))
                                    .text_size(px(12.0))
                                    .text_color(rgb(trace_color(trace.kind, theme)))
                                    .child(trace.title.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(10.0))
                                    .text_color(rgb(theme.foreground_muted))
                                    .opacity(0.55)
                                    .child(trace.created_at.format("%H:%M:%S").to_string()),
                            ),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_size(px(12.0))
                            .line_height(px(19.0))
                            .text_color(rgb(match trace.kind {
                                AgentTraceKind::Warning => theme.warning,
                                AgentTraceKind::Output => theme.foreground,
                                _ => theme.foreground_muted,
                            }))
                            .child(trace.detail.clone()),
                    ),
            ),
    )
}

/// 在消息流末尾渲染结构化报告；发现之间只使用分隔线，不创建独立卡片。
fn render_report_message(
    report: &DiagnosticReport,
    report_path: Option<&str>,
    app: Entity<ArgusApp>,
    scope: Arc<SourceScopeSnapshot>,
    theme: &AppTheme,
) -> impl IntoElement {
    let finding_elements = report
        .findings
        .iter()
        .enumerate()
        .map(|(finding_index, finding)| {
            let evidence_elements = finding
                .evidence
                .iter()
                .enumerate()
                .map(|(evidence_index, evidence)| {
                    let source = scope.source(&evidence.source_ref);
                    let source_id = source.map(|source| source.source_id);
                    let label = source
                        .map(|source| source.relative_path.clone())
                        .unwrap_or_else(|| "来源已失效".to_string());
                    let start_line = evidence.start_line;
                    let navigate_app = app.clone();
                    div()
                        .id((
                            "agent-report-evidence",
                            finding_index * 1000 + evidence_index,
                        ))
                        .mt_2()
                        .text_size(px(10.0))
                        .line_height(px(16.0))
                        .text_color(rgb(if source_id.is_some() {
                            theme.info
                        } else {
                            theme.foreground_muted
                        }))
                        .when(source_id.is_some(), |this| {
                            this.cursor_pointer()
                                .hover(|hover| hover.opacity(0.82))
                                .on_click(move |_, _, app_cx| {
                                    if let Some(source_id) = source_id {
                                        navigate_app.update(app_cx, |main_app, cx| {
                                            main_app.open_ai_evidence(source_id, start_line, cx);
                                            cx.notify();
                                        });
                                    }
                                })
                        })
                        .child(format!(
                            "↳ {} · 第 {}-{} 行 · {}",
                            label, evidence.start_line, evidence.end_line, evidence.rationale
                        ))
                })
                .collect::<Vec<_>>();
            div()
                .mt_4()
                .pt_4()
                .border_t_1()
                .border_color(rgb(theme.border))
                .child(
                    div()
                        .text_size(px(12.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!(
                            "[{} / {}] {}",
                            finding.severity,
                            finding.status.label(),
                            finding.title
                        )),
                )
                .child(
                    div()
                        .mt_2()
                        .text_size(px(12.0))
                        .line_height(px(19.0))
                        .child(finding.analysis.clone()),
                )
                .child(
                    div()
                        .mt_2()
                        .text_size(px(11.0))
                        .line_height(px(17.0))
                        .text_color(rgb(theme.foreground_muted))
                        .child(format!("影响：{}", finding.impact)),
                )
                .child(
                    div()
                        .mt_1()
                        .text_size(px(11.0))
                        .line_height(px(17.0))
                        .text_color(rgb(theme.info))
                        .child(format!("建议：{}", finding.recommendation)),
                )
                .child(
                    div()
                        .mt_2()
                        .text_size(px(10.0))
                        .text_color(rgb(theme.foreground_muted))
                        .child(format!(
                            "置信度 {:.0}% · {} 条证据",
                            finding.confidence * 100.0,
                            finding.evidence.len()
                        )),
                )
                .children(evidence_elements)
        })
        .collect::<Vec<_>>();

    div()
        .w_full()
        .max_w(px(AGENT_STREAM_MAX_WIDTH))
        .flex()
        .gap_3()
        .child(
            div()
                .w(px(20.0))
                .pt(px(2.0))
                .flex_none()
                .flex()
                .justify_center()
                .child(render_icon(ArgusIcon::FileText, theme.info, 14.0)),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_size(px(12.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Argus"),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgb(theme.info))
                                .child("分析报告"),
                        ),
                )
                .child(
                    div()
                        .mt_2()
                        .text_size(px(13.0))
                        .line_height(px(20.0))
                        .child(report.summary.clone()),
                )
                .children(finding_elements)
                .when(!report.limitations.is_empty(), |this| {
                    this.child(
                        div()
                            .mt_4()
                            .text_size(px(11.0))
                            .text_color(rgb(theme.warning))
                            .child(format!("限制：{}", report.limitations.join("；"))),
                    )
                })
                .when_some(report_path.map(str::to_string), |this, path| {
                    this.child(
                        div()
                            .mt_3()
                            .text_size(px(10.0))
                            .text_color(rgb(theme.syntax.comment))
                            .child(format!("报告已保存：{path}")),
                    )
                }),
        )
}

/// 渲染窗口小型操作按钮。
fn action_button(
    id: &'static str,
    label: &'static str,
    primary: bool,
    enabled: bool,
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
        .opacity(if enabled { 1.0 } else { 0.45 })
        .when(enabled, |this| {
            this.cursor_pointer()
                .hover(|hover| hover.opacity(0.82))
                .on_click(on_click)
        })
        .child(label)
}

/// 返回轨迹类型图标。
fn trace_icon(kind: AgentTraceKind) -> ArgusIcon {
    match kind {
        AgentTraceKind::Status => ArgusIcon::Info,
        AgentTraceKind::Model | AgentTraceKind::Reasoning | AgentTraceKind::Output => {
            ArgusIcon::SmartAnalysis
        }
        AgentTraceKind::Tool => ArgusIcon::Settings,
        AgentTraceKind::User => ArgusIcon::ArrowRight,
        AgentTraceKind::Warning => ArgusIcon::Info,
        AgentTraceKind::Report => ArgusIcon::FileText,
    }
}

/// 返回轨迹类型颜色。
fn trace_color(kind: AgentTraceKind, theme: &AppTheme) -> u32 {
    match kind {
        AgentTraceKind::Warning => theme.warning,
        AgentTraceKind::Reasoning | AgentTraceKind::Output | AgentTraceKind::Report => theme.info,
        _ => theme.foreground_muted,
    }
}

/// 返回消息瀑布流中稳定、简短的消息发送方标签。
fn trace_actor_label(kind: AgentTraceKind) -> &'static str {
    match kind {
        AgentTraceKind::Status => "状态",
        AgentTraceKind::Model
        | AgentTraceKind::Reasoning
        | AgentTraceKind::Output
        | AgentTraceKind::Report => "Argus",
        AgentTraceKind::Tool => "工具",
        AgentTraceKind::User => "你",
        AgentTraceKind::Warning => "提示",
    }
}

/// 格式化模型累计 Token 构成；总量已经在指标首行展示，次行只保留输入、输出和可选思考量。
fn format_token_breakdown(budget: AgentBudgetSnapshot) -> String {
    if budget.reasoning_tokens > 0 {
        format!(
            "入 {} · 出 {} · 思考 {}",
            format_compact_tokens(budget.input_tokens),
            format_compact_tokens(budget.output_tokens),
            format_compact_tokens(budget.reasoning_tokens)
        )
    } else {
        format!(
            "入 {} · 出 {}",
            format_compact_tokens(budget.input_tokens),
            format_compact_tokens(budget.output_tokens)
        )
    }
}

/// 格式化最近一轮模型输入的上下文指标，分别返回首行比例和次行容量组成。
fn format_context_metric(
    budget: AgentBudgetSnapshot,
    context_window_tokens: u64,
) -> (String, String) {
    let capacity = format_compact_tokens(context_window_tokens);
    let Some(input_tokens) = budget.latest_input_tokens else {
        return ("上下文 --".to_string(), format!("-- / {capacity}"));
    };
    let percentage = if context_window_tokens == 0 {
        0.0
    } else {
        input_tokens as f64 * 100.0 / context_window_tokens as f64
    };
    (
        format!("上下文 {percentage:.1}%"),
        format!("{} / {capacity}", format_compact_tokens(input_tokens)),
    )
}

/// 根据用户滚轮操作计算消息流是否继续跟随底部。
fn trace_following_after_wheel(
    was_following: bool,
    current_offset: Pixels,
    max_offset: Pixels,
    delta_y: Pixels,
) -> bool {
    if max_offset <= px(0.5) {
        return true;
    }
    // 正向滚轮会把负偏移拉近零，即用户正在查看更早内容，应立即暂停自动跟随。
    if delta_y > px(0.0) {
        return false;
    }
    if delta_y == px(0.0) {
        return was_following;
    }
    let next_offset = (current_offset + delta_y).clamp(-max_offset, px(0.0));
    max_offset + next_offset <= px(AGENT_STREAM_BOTTOM_THRESHOLD)
}

/// 使用 K/M 缩写紧凑展示 Token 容量，避免状态栏因大整数被挤压。
fn format_compact_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

/// 格式化预算字节数。
fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    }
}

/// 只展示会话 ID 前八位，避免标题栏过长。
fn short_id(value: &str) -> String {
    value.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证用户向上查看历史时暂停跟随，只有向下回到底部阈值内才恢复。
    #[test]
    fn trace_following_respects_manual_scroll_position() {
        assert!(!trace_following_after_wheel(
            true,
            px(-200.0),
            px(200.0),
            px(20.0)
        ));
        assert!(!trace_following_after_wheel(
            false,
            px(-100.0),
            px(200.0),
            px(-50.0)
        ));
        assert!(trace_following_after_wheel(
            false,
            px(-180.0),
            px(200.0),
            px(-20.0)
        ));
    }

    /// 验证内容没有溢出时始终视为位于最新位置，避免无意义显示跳转按钮。
    #[test]
    fn trace_following_stays_enabled_without_overflow() {
        assert!(trace_following_after_wheel(
            false,
            px(0.0),
            px(0.0),
            px(20.0)
        ));
    }

    /// 验证上下文指标把比例和容量拆成稳定的两行展示内容。
    #[test]
    fn context_metric_separates_percentage_and_capacity() {
        let budget = AgentBudgetSnapshot {
            latest_input_tokens: Some(16_000),
            ..AgentBudgetSnapshot::default()
        };
        assert_eq!(
            format_context_metric(budget, 128_000),
            ("上下文 12.5%".to_string(), "16.0K / 128.0K".to_string())
        );
    }
}
