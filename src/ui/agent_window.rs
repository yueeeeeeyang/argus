//! 文件职责：渲染 AI 日志分析的独立对话窗口。
//! 创建日期：2026-07-15
//! 修改日期：2026-09-12
//! 作者：Argus 开发团队
//! 主要功能：流式展示模型思考、正文、工具轨迹与 Token 用量，并在底部悬浮文本域中接收会话追加提示。

use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, FocusHandle, FontWeight, IntoElement,
    KeyDownEvent, ListAlignment, ListState, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Render, ScrollHandle, Subscription, Timer, Window, canvas, div, list,
    point, prelude::*, px, rgb,
};
use std::collections::HashSet;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use crate::agent::{
    AgentBudgetSnapshot, AgentEvent, AgentLogProfileMatchSummary, AgentSessionStatus,
    AgentStreamKind, AgentTraceEntry, AgentTraceKind, AgentUserMessage, AgentUserMessageStatus,
    BashApprovalDecision, SourceScopeSnapshot,
};
use crate::app::{ArgusApp, TextInputState, observe_app_theme};
use crate::config::{LogNameMatcherMode, LogNameMatcherTarget};
use crate::fonts::ARGUS_UI_FONT_FAMILY;
use crate::theme::AppTheme;
use crate::ui::components::bash_approval::{
    BashApprovalCard, BashApprovalStatus, bash_approval_reason_label, render_bash_approval_card,
};
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{
    IconButtonSize, render_icon_button, render_round_icon_button,
};
use crate::ui::components::input::{
    InputAccessory, InputPointerAction, InputPointerEvent, NativeInput, Textarea,
    TextareaAccessoryPosition, TextareaScrollState, TextareaStyle, render_textarea,
};
use crate::ui::components::input_behavior::{LocalInputAction, handle_local_input_key};
use crate::ui::components::loading_spinner::render_loading_spinner;
use crate::ui::components::markdown::{MarkdownStyle, render_markdown};
use crate::ui::components::scrollbar::{scrollbar_metrics, scrollbar_scroll_for_drag};
use crate::ui::components::window_title_bar::render_window_title_bar;
use crate::ui::custom_title_bar::TITLE_BAR_HEIGHT;

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
/// 消息虚拟列表在可见区域上下额外渲染的高度，避免快速滚动时边缘内容闪烁。
const AGENT_STREAM_OVERDRAW: f32 = 360.0;
/// 消息瀑布流纵向滚动条滑块宽度；不绘制轨道背景。
const AGENT_STREAM_SCROLLBAR_THUMB_WIDTH: f32 = 4.0;
/// 消息瀑布流滚动条上下留白。
const AGENT_STREAM_SCROLLBAR_PADDING: f32 = 4.0;
/// 消息瀑布流滚动条最小滑块高度，保证长会话中仍可拖拽。
const AGENT_STREAM_SCROLLBAR_MIN_THUMB: f32 = 28.0;
/// 后台流式事件合并窗口，限制界面更新频率不超过一帧一次。
const AGENT_EVENT_BATCH_INTERVAL: Duration = Duration::from_millis(16);

/// 虚拟消息列表中的稳定渲染单元。
///
/// 条目同时承担差异键职责：流式文本长度、活动状态或工具展开状态变化时，只有对应行会被
/// `ListState::splice` 标记为需要重新测量，历史消息继续复用已缓存高度。
#[derive(Clone, Debug, Eq, PartialEq)]
enum AgentStreamItem {
    /// 用户最初提交的问题。
    Question,
    /// 单条普通轨迹。
    Trace {
        /// 原始轨迹索引。
        trace_index: usize,
        /// 轨迹创建时间生成的稳定会话内标识。
        trace_id: i64,
        /// 标题和正文当前 UTF-8 字节数，用作内容修订号。
        content_bytes: usize,
        /// 是否显示正在执行动画。
        is_active: bool,
    },
    /// 一组相邻工具轨迹。
    ToolGroup {
        /// 组在原始轨迹中的起始索引。
        start: usize,
        /// 组在原始轨迹中的开区间结束索引。
        end: usize,
        /// 组首轨迹生成的稳定会话内标识。
        group_id: i64,
        /// 组内当前总文本字节数，用作内容修订号。
        content_bytes: usize,
        /// 是否展开全部调用明细。
        is_expanded: bool,
        /// 是否显示正在执行动画。
        is_active: bool,
    },
    /// 一条 bash 审批确认卡片。
    BashApproval {
        /// 卡片在审批列表中的索引。
        approval_index: usize,
        /// 当前审批状态；状态变化驱动虚拟行重新测量。
        status: BashApprovalStatus,
    },
    /// 消息流末尾保留的呼吸空间。
    Spacer,
}

/// Agent 独立窗口根视图。
pub(crate) struct AgentWindow {
    /// 当前主题快照。
    theme: AppTheme,
    /// 会话随机 ID。
    session_id: String,
    /// 用户初始问题。
    question: String,
    /// 当前状态机状态。
    status: AgentSessionStatus,
    /// 用户提交问题并开始来源扫描的时刻，用于计算包含预处理在内的整体耗时。
    analysis_started_at: Instant,
    /// 进入终态时冻结的整体耗时；运行中保持为空并使用单调时钟实时计算。
    analysis_finished_elapsed_seconds: Option<u64>,
    /// 本次会话所选模型的上下文窗口 Token 数。
    context_window_tokens: u64,
    /// 增量轻量轨迹，不保存完整工具输出或日志原文。
    traces: Arc<Vec<Arc<AgentTraceEntry>>>,
    /// 用户主动展开的连续工具轨迹组，键由组首条轨迹时间生成。
    expanded_tool_groups: HashSet<i64>,
    /// 可变高度消息虚拟列表状态，只布局可见消息及少量预渲染区域。
    trace_list: ListState,
    /// 上一帧消息条目差异键，用于精准失效发生变化的虚拟行。
    trace_items: Vec<AgentStreamItem>,
    /// 是否自动跟随消息流最新位置；用户主动上滚后关闭，回到底部后恢复。
    is_trace_following: bool,
    /// 是否已经给虚拟列表注册滚动状态监听。
    has_registered_trace_scroll_handler: bool,
    /// 用户拖动消息流滚动条时，指针相对滑块顶部的偏移。
    trace_scrollbar_drag_offset: Option<Pixels>,
    /// 最新资源预算快照。
    budget: AgentBudgetSnapshot,
    /// 会话内累计的 bash 审批卡片；按创建时间与轨迹交错展示。
    bash_approvals: Vec<BashApprovalCard>,
    /// 发送给后台工具的审批答复通道。
    bash_decision_sender: async_channel::Sender<BashApprovalDecision>,
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
    /// 提示入队和终止阶段关闭入口共用的线性化门闩。
    user_message_gate: Arc<std::sync::Mutex<bool>>,
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
        bash_decision_sender: async_channel::Sender<BashApprovalDecision>,
        context_window_tokens: u64,
        analysis_started_at: Instant,
        cx: &mut Context<Self>,
    ) -> Self {
        let _theme_observer = observe_app_theme(cx, &app, theme.clone(), |view, next_theme, _| {
            view.theme = next_theme.clone();
        });
        cx.spawn(async move |view, cx| {
            while let Ok(first_event) = event_receiver.recv().await {
                let mut events = Vec::with_capacity(32);
                events.push(first_event);
                // 给同一显示帧内的 Token 留出极短合并窗口，避免模型高吞吐时每个碎片触发一次重绘。
                Timer::after(AGENT_EVENT_BATCH_INTERVAL).await;
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
                        // 同一批 Token / 工具事件只做一次列表差异同步，避免在单帧内重复失效高度缓存。
                        window.sync_trace_items();
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        // 顶部整体耗时必须独立于模型和工具事件每秒刷新；终态后立即退出，避免空闲窗口常驻任务。
        cx.spawn(async move |view, cx| {
            loop {
                Timer::after(Duration::from_secs(1)).await;
                let should_continue = view
                    .update(cx, |window, cx| {
                        if window.status.is_terminal() {
                            false
                        } else {
                            cx.notify();
                            true
                        }
                    })
                    .unwrap_or(false);
                if !should_continue {
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
            theme,
            session_id,
            question,
            status: AgentSessionStatus::Created,
            analysis_started_at,
            analysis_finished_elapsed_seconds: None,
            context_window_tokens,
            traces: Arc::new(vec![Arc::new(AgentTraceEntry::new(
                AgentTraceKind::Status,
                "会话已创建",
                source_scan_summary,
            ))]),
            expanded_tool_groups: HashSet::new(),
            trace_list: ListState::new(2, ListAlignment::Bottom, px(AGENT_STREAM_OVERDRAW)),
            trace_items: vec![
                AgentStreamItem::Question,
                AgentStreamItem::Trace {
                    trace_index: 0,
                    trace_id: 0,
                    content_bytes: 0,
                    is_active: true,
                },
            ],
            is_trace_following: true,
            has_registered_trace_scroll_handler: false,
            trace_scrollbar_drag_offset: None,
            budget: AgentBudgetSnapshot::default(),
            bash_approvals: Vec::new(),
            bash_decision_sender,
            message_input,
            message_scroll: ScrollHandle::new(),
            message_scroll_state: TextareaScrollState::new(),
            user_messages: Vec::new(),
            user_message_sender,
            cancellation,
            pending_user_messages,
            user_message_gate,
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
                if status.is_terminal() && self.analysis_finished_elapsed_seconds.is_none() {
                    self.analysis_finished_elapsed_seconds =
                        Some(self.analysis_started_at.elapsed().as_secs());
                }
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
            // 交互助手完成事件只由右侧面板消费；本窗口不会收到它。
            AgentEvent::AssistantCompleted { .. } => {}
            // 统一循环重试前丢弃本次尝试尚未完成的流式正文，避免重试内容重复拼接。
            AgentEvent::AssistantAttemptReset => {
                self.drop_incomplete_stream_text();
            }
            AgentEvent::BashApprovalRequired {
                request_id,
                command,
            } => {
                self.bash_approvals
                    .push(BashApprovalCard::pending(request_id, command));
            }
            AgentEvent::BashApprovalOutcome {
                request_id,
                approved,
                reason,
            } => {
                if let Some(card) = self
                    .bash_approvals
                    .iter_mut()
                    .find(|card| card.request_id == request_id)
                    && card.status == BashApprovalStatus::Pending
                {
                    card.status = if approved {
                        BashApprovalStatus::Approved
                    } else {
                        BashApprovalStatus::Denied(bash_approval_reason_label(&reason))
                    };
                }
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

    /// 丢弃末尾连续的思考与正文轨迹；模型重试会从空白重新流式输出。
    fn drop_incomplete_stream_text(&mut self) {
        let traces = Arc::make_mut(&mut self.traces);
        while let Some(last) = traces.last() {
            if matches!(
                last.kind,
                AgentTraceKind::Reasoning | AgentTraceKind::Output
            ) {
                traces.pop();
            } else {
                break;
            }
        }
    }

    /// 返回会话是否已经进入终态；供应用层决定关闭重建或拒绝并发会话。
    pub(crate) fn is_session_terminal(&self) -> bool {
        self.status.is_terminal()
    }

    /// 回传用户对一条 bash 审批的答复，并即时更新卡片状态。
    fn resolve_bash_approval(&mut self, request_id: &str, approved: bool) {
        let Some(card) = self
            .bash_approvals
            .iter_mut()
            .find(|card| card.request_id == request_id)
        else {
            return;
        };
        if card.status != BashApprovalStatus::Pending {
            return;
        }
        card.status = if approved {
            BashApprovalStatus::Approved
        } else {
            BashApprovalStatus::Denied("你拒绝了这条命令".to_string())
        };
        let _ = self.bash_decision_sender.try_send(BashApprovalDecision {
            request_id: request_id.to_string(),
            approved,
        });
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
        if let Some(last_trace) = Arc::make_mut(&mut self.traces).last_mut() {
            let last_trace = Arc::make_mut(last_trace);
            if last_trace.kind == trace_kind {
                last_trace.detail.push_str(&delta);
                return;
            }
        }
        self.push_trace(AgentTraceEntry::new(trace_kind, title, delta));
    }

    /// 追加轨迹并丢弃最旧的超限条目。
    fn push_trace(&mut self, trace: AgentTraceEntry) {
        let traces = Arc::make_mut(&mut self.traces);
        traces.push(Arc::new(trace));
        if traces.len() > AGENT_TRACE_MAX_COUNT {
            let drain_count = traces.len() - AGENT_TRACE_MAX_COUNT;
            traces.drain(0..drain_count);
        }
    }

    /// 跳转到消息流底部并恢复后续流式事件自动跟随。
    fn jump_trace_to_latest(&mut self) {
        self.is_trace_following = true;
        let max_offset = self.trace_list.max_offset_for_scrollbar().height;
        self.trace_list
            .set_offset_from_scrollbar(point(px(0.0), -max_offset));
    }

    /// 根据当前轨迹、审批卡片和展开状态更新虚拟列表条目，并只失效变化的连续区间。
    fn sync_trace_items(&mut self) {
        let next_items = build_agent_stream_items(
            &self.traces,
            self.status,
            &self.expanded_tool_groups,
            &self.bash_approvals,
        );
        for (old_range, replacement_count) in
            changed_stream_item_ranges(&self.trace_items, &next_items)
        {
            self.trace_list.splice(old_range, replacement_count);
        }
        self.trace_items = next_items;
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
        // 校验入口状态和写入有界队列必须持有同一门闩，避免会话终止竞态静默遗漏消息。
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
        self.sync_trace_items();
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
        self.sync_trace_items();
    }

    /// 返回从用户提交问题开始计算的整体分析耗时，包含来源扫描、模型和工具阶段。
    fn overall_elapsed_seconds(&self) -> u64 {
        self.analysis_finished_elapsed_seconds
            .unwrap_or_else(|| self.analysis_started_at.elapsed().as_secs())
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
        self.sync_trace_items();
    }
}

/// 构建当前消息流的轻量虚拟条目，不复制轨迹正文。
fn build_agent_stream_items(
    traces: &[Arc<AgentTraceEntry>],
    status: AgentSessionStatus,
    expanded_tool_groups: &HashSet<i64>,
    bash_approvals: &[BashApprovalCard],
) -> Vec<AgentStreamItem> {
    let mut items = Vec::with_capacity(
        traces
            .len()
            .saturating_add(bash_approvals.len())
            .saturating_add(2),
    );
    items.push(AgentStreamItem::Question);
    let active_trace_index = if status.is_terminal() {
        None
    } else {
        traces.iter().rposition(|trace| {
            matches!(
                trace.kind,
                AgentTraceKind::Status
                    | AgentTraceKind::Reasoning
                    | AgentTraceKind::Output
                    | AgentTraceKind::Tool
            )
        })
    };

    // 审批卡片按创建时间插入对应轨迹之前；索引单调递增，保证差异键稳定。
    let mut pending_approval_index = 0usize;
    let mut flush_approvals_before =
        |items: &mut Vec<AgentStreamItem>, boundary: chrono::DateTime<chrono::Utc>| {
            while let Some(card) = bash_approvals.get(pending_approval_index)
                && chrono::DateTime::<chrono::Utc>::from(card.created_at) <= boundary
            {
                items.push(AgentStreamItem::BashApproval {
                    approval_index: pending_approval_index,
                    status: card.status.clone(),
                });
                pending_approval_index += 1;
            }
        };

    let mut trace_index = 0;
    while trace_index < traces.len() {
        let trace = &traces[trace_index];
        // 模型请求统计只在顶部信息栏展示；同时隐藏后台重试前残留的模型轨迹，
        // 确保重试或事件竞态不会让请求行重新出现。
        if trace.kind == AgentTraceKind::Model {
            trace_index += 1;
            continue;
        }
        flush_approvals_before(&mut items, trace.created_at);
        if trace.kind != AgentTraceKind::Tool {
            items.push(AgentStreamItem::Trace {
                trace_index,
                trace_id: tool_group_id(trace),
                content_bytes: trace.title.len().saturating_add(trace.detail.len()),
                is_active: active_trace_index == Some(trace_index),
            });
            trace_index += 1;
            continue;
        }

        let start = trace_index;
        let mut content_bytes = 0usize;
        while trace_index < traces.len() && traces[trace_index].kind == AgentTraceKind::Tool {
            content_bytes = content_bytes
                .saturating_add(traces[trace_index].title.len())
                .saturating_add(traces[trace_index].detail.len());
            trace_index += 1;
        }
        let group_id = tool_group_id(&traces[start]);
        items.push(AgentStreamItem::ToolGroup {
            start,
            end: trace_index,
            group_id,
            content_bytes,
            is_expanded: expanded_tool_groups.contains(&group_id),
            is_active: active_trace_index
                .is_some_and(|active_index| (start..trace_index).contains(&active_index)),
        });
    }
    // 晚于全部轨迹的审批卡片追加在末尾。
    while pending_approval_index < bash_approvals.len() {
        items.push(AgentStreamItem::BashApproval {
            approval_index: pending_approval_index,
            status: bash_approvals[pending_approval_index].status.clone(),
        });
        pending_approval_index += 1;
    }

    items.push(AgentStreamItem::Spacer);
    items
}

/// 找出新旧虚拟条目的变化区间；等长更新按离散区间失效，避免完成态波及中间内容。
fn changed_stream_item_ranges(
    previous: &[AgentStreamItem],
    next: &[AgentStreamItem],
) -> Vec<(std::ops::Range<usize>, usize)> {
    if previous.len() == next.len() {
        let mut ranges = Vec::new();
        let mut index = 0usize;
        while index < previous.len() {
            if previous[index] == next[index] {
                index += 1;
                continue;
            }
            let start = index;
            while index < previous.len() && previous[index] != next[index] {
                index += 1;
            }
            ranges.push((start..index, index - start));
        }
        return ranges;
    }

    let common_prefix = previous
        .iter()
        .zip(next)
        .take_while(|(left, right)| left == right)
        .count();
    if common_prefix == previous.len() && common_prefix == next.len() {
        return Vec::new();
    }

    let remaining_previous = previous.len().saturating_sub(common_prefix);
    let remaining_next = next.len().saturating_sub(common_prefix);
    let common_suffix = previous[common_prefix..]
        .iter()
        .rev()
        .zip(next[common_prefix..].iter().rev())
        .take_while(|(left, right)| left == right)
        .count()
        .min(remaining_previous)
        .min(remaining_next);
    let old_end = previous.len().saturating_sub(common_suffix);
    let new_end = next.len().saturating_sub(common_suffix);
    vec![(common_prefix..old_end, new_end - common_prefix)]
}

/// 格式化工作区清单固化结果和逐规则命中统计，供会话首条状态消息展示。
fn format_source_scan_summary(
    scope: &SourceScopeSnapshot,
    match_summaries: &[AgentLogProfileMatchSummary],
) -> String {
    let mut lines = vec![format!(
        "工作区清单已固化，共 {} 个日志文件，匹配 {} 种日志类型说明；模型可调用 \
        list_loaded_sources、read_file 和受控 bash 在工作目录内取证。",
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
    /// 渲染轨迹、预算和底部悬浮对话框。
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
        if !self.has_registered_trace_scroll_handler {
            let scroll_entity = entity.clone();
            self.trace_list.set_scroll_handler(move |event, _, app_cx| {
                scroll_entity.update(app_cx, |view, _| {
                    view.is_trace_following = !event.is_scrolled;
                });
            });
            self.has_registered_trace_scroll_handler = true;
        }
        self.sync_trace_items();
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
        let can_cancel_session = !status.is_terminal() && status != AgentSessionStatus::Cancelling;
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
                TITLE_BAR_HEIGHT,
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
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.0))
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(render_budget_bar(
                        self.budget,
                        self.context_window_tokens,
                        self.overall_elapsed_seconds(),
                        status,
                        &theme,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(0.0))
                            .overflow_hidden()
                            .child(render_message_stream(
                                &self.question,
                                self.traces.clone(),
                                self.trace_items.clone(),
                                self.bash_approvals.clone(),
                                self.trace_list.clone(),
                                entity.clone(),
                                &theme,
                            )),
                    )
                    .child(div().h(px(128.0)).flex_none())
                    .when(show_jump_to_latest, |this| {
                        this.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom(px(138.0))
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
                                    .child(
                                        div()
                                            .relative()
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
                                            .when(can_cancel_session, |this| {
                                                this.child(
                                                    div()
                                                        .absolute()
                                                        .right(px(32.0))
                                                        .bottom(px(4.0))
                                                        .child(render_icon_button(
                                                            "agent-cancel",
                                                            ArgusIcon::Stop,
                                                            "取消分析",
                                                            false,
                                                            IconButtonSize::Tiny,
                                                            &theme,
                                                            move |_, _, app_cx| {
                                                                app_cx.stop_propagation();
                                                                cancel_entity.update(app_cx, |view, cx| {
                                                                    view.cancel_session();
                                                                    cx.notify();
                                                                });
                                                            },
                                                        )),
                                                )
                                            }),
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

/// 把秒数压缩为易读的耗时文本。
fn format_stage_duration(seconds: u64) -> String {
    match seconds {
        0 => "< 1 秒".to_string(),
        1..=59 => format!("{seconds} 秒"),
        60..=3599 => format!("{} 分 {} 秒", seconds / 60, seconds % 60),
        _ => format!(
            "{} 小时 {} 分 {} 秒",
            seconds / 3600,
            seconds % 3600 / 60,
            seconds % 60
        ),
    }
}

/// 格式化包含来源扫描在内的整体分析耗时，避免用户解读原始秒数。
fn format_analysis_duration(seconds: u64) -> String {
    format_stage_duration(seconds)
}

/// 渲染资源预算条。
fn render_budget_bar(
    budget: AgentBudgetSnapshot,
    context_window_tokens: u64,
    overall_elapsed_seconds: u64,
    status: AgentSessionStatus,
    theme: &AppTheme,
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
                "扫描 {} · 原文 {}",
                format_bytes(budget.local_scan_bytes),
                format_bytes(budget.raw_log_bytes)
            ),
            theme,
        )))
        .child(render_budget_divider(theme))
        .child(div().w(px(160.0)).flex_none().child(render_budget_metric(
            format!(
                "总耗时 {}",
                format_analysis_duration(overall_elapsed_seconds)
            ),
            status.label().to_string(),
            theme,
        )))
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

/// 渲染单列消息瀑布流。
fn render_message_stream(
    question: &str,
    traces: Arc<Vec<Arc<AgentTraceEntry>>>,
    items: Vec<AgentStreamItem>,
    bash_approvals: Vec<BashApprovalCard>,
    list_state: ListState,
    agent_window: Entity<AgentWindow>,
    theme: &AppTheme,
) -> impl IntoElement {
    let question = Arc::<str>::from(question);
    let items = Arc::new(items);
    let render_items = items.clone();
    let render_traces = traces.clone();
    let render_approvals = Arc::new(bash_approvals);
    let render_agent_window = agent_window.clone();
    let render_theme = theme.clone();
    let render_question = question.clone();

    div()
        .id("agent-message-stream")
        .relative()
        .h_full()
        .w_full()
        .min_w(px(0.0))
        .child(
            list(list_state.clone(), move |index, _, _| {
                let Some(item) = render_items.get(index) else {
                    return div().into_any_element();
                };
                render_agent_stream_item(
                    item,
                    &render_question,
                    &render_traces,
                    &render_approvals,
                    render_agent_window.clone(),
                    &render_theme,
                )
            })
            .size_full(),
        )
        .child(render_agent_stream_scrollbar(
            list_state,
            agent_window,
            theme,
        ))
}

/// 仅为虚拟列表当前请求的索引构造消息元素，屏幕外内容不会进入本帧布局树。
fn render_agent_stream_item(
    item: &AgentStreamItem,
    question: &str,
    traces: &[Arc<AgentTraceEntry>],
    bash_approvals: &[BashApprovalCard],
    agent_window: Entity<AgentWindow>,
    theme: &AppTheme,
) -> AnyElement {
    match *item {
        AgentStreamItem::Question => render_question_message(question, theme).into_any_element(),
        AgentStreamItem::BashApproval { approval_index, .. } => {
            bash_approvals.get(approval_index).map_or_else(
                || div().into_any_element(),
                |card| {
                    let decision_window = agent_window.clone();
                    let decision_request_id = card.request_id.clone();
                    div()
                        .w_full()
                        .px_6()
                        .child(render_bash_approval_card(
                            &card.request_id,
                            &card.command,
                            &card.status,
                            theme,
                            AGENT_STREAM_MAX_WIDTH,
                            move |approved, _, _, app_cx| {
                                decision_window.update(app_cx, |window, cx| {
                                    window.resolve_bash_approval(&decision_request_id, approved);
                                    cx.notify();
                                });
                            },
                        ))
                        .into_any_element()
                },
            )
        }
        AgentStreamItem::Trace {
            trace_index,
            is_active,
            ..
        } => traces.get(trace_index).map_or_else(
            || div().into_any_element(),
            |trace| render_trace_message(trace, trace_index, is_active, theme).into_any_element(),
        ),
        AgentStreamItem::ToolGroup {
            start,
            end,
            group_id,
            is_expanded,
            is_active,
            ..
        } => traces.get(start..end).map_or_else(
            || div().into_any_element(),
            |group| {
                render_tool_group_message(
                    group,
                    start,
                    group_id,
                    is_expanded,
                    is_active,
                    agent_window,
                    theme,
                )
                .into_any_element()
            },
        ),
        AgentStreamItem::Spacer => div().h(px(28.0)).flex_none().into_any_element(),
    }
}

/// 渲染用户问题消息。
fn render_question_message(question: &str, theme: &AppTheme) -> impl IntoElement {
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
                        .child(div().mt_1().child(render_markdown(
                            question,
                            MarkdownStyle {
                                font_size: 13.0,
                                line_height: 20.0,
                                color: theme.foreground,
                            },
                            theme,
                        ))),
                ),
        )
}

/// 根据虚拟列表已测量高度绘制纵向滚动条滑块；内容未溢出时完全隐藏。
fn render_agent_stream_scrollbar(
    list_state: ListState,
    agent_window: Entity<AgentWindow>,
    theme: &AppTheme,
) -> AnyElement {
    let viewport_bounds = list_state.viewport_bounds();
    let max_scroll = list_state.max_offset_for_scrollbar().height;
    let current_scroll = -list_state.scroll_px_offset_for_scrollbar().y;
    let content_height = viewport_bounds.size.height + max_scroll;
    let Some(metrics) = scrollbar_metrics(
        viewport_bounds.size.height,
        content_height,
        current_scroll,
        AGENT_STREAM_SCROLLBAR_PADDING,
        AGENT_STREAM_SCROLLBAR_MIN_THUMB,
    ) else {
        return render_agent_stream_scrollbar_sentinel(list_state, agent_window);
    };

    let mouse_state = list_state.clone();
    div()
        .id("agent-message-stream-scrollbar")
        .absolute()
        .top(metrics.thumb_start)
        .right(px(AGENT_STREAM_SCROLLBAR_PADDING))
        .w(px(AGENT_STREAM_SCROLLBAR_THUMB_WIDTH))
        .h(metrics.thumb_length)
        .rounded_lg()
        .bg(rgb(theme.foreground_muted))
        .opacity(0.5)
        .hover(|thumb| thumb.opacity(0.8))
        .cursor_pointer()
        .occlude()
        .child(
            canvas(
                |_, _, _| (),
                move |thumb_bounds, _, window: &mut Window, _| {
                    window.on_mouse_event({
                        let agent_window = agent_window.clone();
                        let list_state = mouse_state.clone();
                        move |event: &MouseDownEvent, phase, _, app_cx| {
                            if !phase.bubble()
                                || event.button != MouseButton::Left
                                || !thumb_bounds.contains(&event.position)
                            {
                                return;
                            }
                            list_state.scrollbar_drag_started();
                            agent_window.update(app_cx, |view, _| {
                                view.trace_scrollbar_drag_offset =
                                    Some(event.position.y - thumb_bounds.top());
                            });
                            app_cx.stop_propagation();
                            app_cx.notify(agent_window.entity_id());
                        }
                    });

                    window.on_mouse_event({
                        let agent_window = agent_window.clone();
                        let list_state = mouse_state.clone();
                        move |event: &MouseUpEvent, phase, _, app_cx| {
                            if !phase.bubble() || event.button != MouseButton::Left {
                                return;
                            }
                            let handled = agent_window.update(app_cx, |view, _| {
                                view.trace_scrollbar_drag_offset.take().is_some()
                            });
                            if handled {
                                list_state.scrollbar_drag_ended();
                                app_cx.stop_propagation();
                                app_cx.notify(agent_window.entity_id());
                            }
                        }
                    });

                    window.on_mouse_event({
                        let agent_window = agent_window.clone();
                        let list_state = mouse_state.clone();
                        move |event: &MouseMoveEvent, phase, _, app_cx| {
                            if !phase.bubble() || !event.dragging() {
                                return;
                            }
                            let Some(cursor_offset) =
                                agent_window.read(app_cx).trace_scrollbar_drag_offset
                            else {
                                return;
                            };
                            let pointer = event.position.y - viewport_bounds.top();
                            let scroll =
                                scrollbar_scroll_for_drag(pointer, cursor_offset, &metrics);
                            list_state.set_offset_from_scrollbar(point(px(0.0), -scroll));
                            agent_window.update(app_cx, |view, _| {
                                view.is_trace_following = metrics.max_scroll - scroll <= px(0.5);
                            });
                            app_cx.stop_propagation();
                            app_cx.notify(agent_window.entity_id());
                        }
                    });
                },
            )
            .size_full(),
        )
        .into_any_element()
}

/// 首帧列表尚未完成测量时使用透明哨兵，在确认内容溢出后触发下一帧显示滑块。
fn render_agent_stream_scrollbar_sentinel(
    list_state: ListState,
    agent_window: Entity<AgentWindow>,
) -> AnyElement {
    canvas(
        |_, _, _| (),
        move |_, _, _, app_cx: &mut App| {
            if list_state.viewport_bounds().size.height > px(0.0)
                && list_state.max_offset_for_scrollbar().height > px(0.0)
            {
                app_cx.notify(agent_window.entity_id());
            }
        },
    )
    .absolute()
    .size_full()
    .into_any_element()
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
    traces: &[Arc<AgentTraceEntry>],
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
    let detail_color = match trace.kind {
        AgentTraceKind::Warning => theme.warning,
        AgentTraceKind::Output => theme.foreground,
        _ => theme.foreground_muted,
    };
    let detail = if matches!(
        trace.kind,
        AgentTraceKind::Reasoning | AgentTraceKind::Output | AgentTraceKind::User
    ) {
        render_markdown(
            &trace.detail,
            MarkdownStyle {
                font_size: 12.0,
                line_height: 19.0,
                color: detail_color,
            },
            theme,
        )
    } else {
        div()
            .text_size(px(12.0))
            .line_height(px(19.0))
            .text_color(rgb(detail_color))
            .child(trace.detail.clone())
            .into_any_element()
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
                    .child(div().mt_1().child(detail)),
            ),
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
    }
}

/// 返回轨迹类型颜色。
fn trace_color(kind: AgentTraceKind, theme: &AppTheme) -> u32 {
    match kind {
        AgentTraceKind::Warning => theme.warning,
        AgentTraceKind::Reasoning | AgentTraceKind::Output => theme.info,
        _ => theme.foreground_muted,
    }
}

/// 返回消息瀑布流中稳定、简短的消息发送方标签。
fn trace_actor_label(kind: AgentTraceKind) -> &'static str {
    match kind {
        AgentTraceKind::Status => "状态",
        AgentTraceKind::Model | AgentTraceKind::Reasoning | AgentTraceKind::Output => "Argus",
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

    /// 验证虚拟列表只失效发生变化的中间行，避免流式增量让历史消息重新测量。
    #[test]
    fn changed_stream_items_only_replace_modified_range() {
        let previous = vec![
            AgentStreamItem::Question,
            AgentStreamItem::Trace {
                trace_index: 0,
                trace_id: 1,
                content_bytes: 10,
                is_active: true,
            },
            AgentStreamItem::Spacer,
        ];
        let mut next = previous.clone();
        next[1] = AgentStreamItem::Trace {
            trace_index: 0,
            trace_id: 1,
            content_bytes: 18,
            is_active: true,
        };
        assert_eq!(
            changed_stream_item_ranges(&previous, &next),
            vec![(1..2, 1)]
        );

        next.push(AgentStreamItem::Spacer);
        assert_eq!(
            changed_stream_item_ranges(&previous, &next),
            vec![(1..2, 2)]
        );
    }

    /// 验证模型请求边界不会进入分析消息瀑布流，模型思考与可见输出仍正常保留。
    #[test]
    fn model_request_traces_are_hidden_from_analysis_stream() {
        let traces = vec![
            Arc::new(AgentTraceEntry::new(
                AgentTraceKind::Status,
                "分析中",
                "会话已启动",
            )),
            Arc::new(AgentTraceEntry::new(
                AgentTraceKind::Model,
                "模型请求 #1",
                "正在请求模型",
            )),
            Arc::new(AgentTraceEntry::new(
                AgentTraceKind::Reasoning,
                "思考过程",
                "正在分析证据",
            )),
        ];
        let items = build_agent_stream_items(
            &traces,
            AgentSessionStatus::Investigating,
            &HashSet::new(),
            &[],
        );
        assert!(
            items
                .iter()
                .any(|item| matches!(item, AgentStreamItem::Trace { trace_index: 0, .. }))
        );
        assert!(
            items
                .iter()
                .any(|item| matches!(item, AgentStreamItem::Trace { trace_index: 2, .. }))
        );
        assert!(
            !items
                .iter()
                .any(|item| matches!(item, AgentStreamItem::Trace { trace_index: 1, .. }))
        );
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

    /// 验证整体耗时按秒、分、小时档位压缩为易读文本。
    #[test]
    fn analysis_duration_formats_compact_text() {
        assert_eq!(format_analysis_duration(0), "< 1 秒");
        assert_eq!(format_analysis_duration(7), "7 秒");
        assert_eq!(format_analysis_duration(61), "1 分 1 秒");
        assert_eq!(format_analysis_duration(3_661), "1 小时 1 分 1 秒");
    }
}
