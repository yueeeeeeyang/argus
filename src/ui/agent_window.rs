//! 文件职责：渲染 AI 日志分析的独立轨迹与报告窗口。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-16
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
    AgentAnalysisStage, AgentAnalysisStageEvent, AgentAnalysisStageStatus, AgentBudgetSnapshot,
    AgentEvent, AgentLogProfileMatchSummary, AgentSessionStatus, AgentStreamKind, AgentTraceEntry,
    AgentTraceKind, AgentUserMessage, AgentUserMessageStatus, DiagnosticFinding, DiagnosticReport,
    SourceScopeSnapshot,
};
use crate::app::{ArgusApp, TextInputState, observe_app_theme};
use crate::config::{LogNameMatcherMode, LogNameMatcherTarget};
use crate::fonts::{ARGUS_LOG_FONT_FAMILY, ARGUS_UI_FONT_FAMILY};
use crate::theme::AppTheme;
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
/// 分析窗口右侧悬浮时间线卡片宽度。
const AGENT_STAGE_CARD_WIDTH: f32 = 300.0;
/// 悬浮时间线与窗口右边缘及主消息区之间的留白。
const AGENT_STAGE_CARD_GAP: f32 = 16.0;
/// 时间线从用量栏下方开始悬浮，避免遮挡顶部状态信息。
const AGENT_STAGE_CARD_TOP: f32 = 64.0;
/// 消息虚拟列表在可见区域上下额外渲染的高度，避免快速滚动时边缘内容闪烁。
const AGENT_STREAM_OVERDRAW: f32 = 360.0;
/// 消息瀑布流纵向滚动条滑块宽度；不绘制轨道背景。
const AGENT_STREAM_SCROLLBAR_THUMB_WIDTH: f32 = 4.0;
/// 消息瀑布流滚动条上下留白。
const AGENT_STREAM_SCROLLBAR_PADDING: f32 = 4.0;
/// 消息瀑布流滚动条最小滑块高度，保证长会话中仍可拖拽。
const AGENT_STREAM_SCROLLBAR_MIN_THUMB: f32 = 28.0;
/// 报告内容逐步呈现的刷新间隔。
const REPORT_STREAM_INTERVAL: Duration = Duration::from_millis(32);
/// 每次刷新最多新增的 Unicode 字符数，降低长报告流式布局频率。
const REPORT_STREAM_CHARS_PER_TICK: usize = 192;
/// 后台流式事件合并窗口，限制界面更新频率不超过一帧一次。
const AGENT_EVENT_BATCH_INTERVAL: Duration = Duration::from_millis(16);
/// 没有形成结构化发现时在“问题分析”部分显示的保守说明。
const EMPTY_FINDINGS_MESSAGE: &str = "当前分析未形成经过证据确认的问题发现。";

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
    /// 报告卡片顶部及问题描述。
    ReportHeader {
        /// 当前可见的问题描述字符数。
        visible_chars: usize,
        /// 报告流是否完成。
        is_complete: bool,
    },
    /// 报告的问题分析分区标题；无发现时同时承载保守说明。
    ReportAnalysisHeader {
        /// 无发现说明当前可见的字符数。
        visible_empty_message_chars: usize,
        /// 是否已经流式推进到问题分析分区。
        is_visible: bool,
    },
    /// 报告中的一条问题发现。
    ReportFinding {
        /// 发现索引。
        finding_index: usize,
        /// 当前发现范围内可见的字符数。
        visible_chars: usize,
    },
    /// 报告结论、建议、限制及保存位置。
    ReportFooter {
        /// 当前结论范围内可见的字符数。
        visible_chars: usize,
        /// 报告流是否完成。
        is_complete: bool,
        /// 是否已经流式推进到结论分区。
        is_visible: bool,
    },
    /// 报告生成前保留的末尾呼吸空间。
    Spacer,
}

/// 右侧悬浮时间线卡片中一个分析阶段的轻量视图状态。
///
/// 后台只发送结构化阶段、结果摘要与已耗时；界面不保存该阶段的思考或工具明细，避免卡片与
/// 消息瀑布流重复。运行起点仅用于在终止事件到达时补齐最后一段阶段耗时。
struct AgentStageViewState {
    /// 固定分析阶段。
    stage: AgentAnalysisStage,
    /// 当前阶段结果。
    status: AgentAnalysisStageStatus,
    /// 后台最后确认的阶段耗时秒数。
    elapsed_seconds: u64,
    /// 阶段完成后的简短结果摘要。
    result_summary: Option<String>,
    /// 本地收到运行事件的时刻；阶段结束时用于补齐事件间隔。
    running_since: Option<Instant>,
}

impl AgentStageViewState {
    /// 创建一个尚未开始的固定阶段视图状态。
    fn pending(stage: AgentAnalysisStage) -> Self {
        Self {
            stage,
            status: AgentAnalysisStageStatus::Pending,
            elapsed_seconds: 0,
            result_summary: None,
            running_since: None,
        }
    }

    /// 应用后台阶段快照，同一运行阶段的重复快照不会重置本地计时起点。
    fn apply(&mut self, event: AgentAnalysisStageEvent) {
        if self.status != AgentAnalysisStageStatus::Running
            || event.status != AgentAnalysisStageStatus::Running
        {
            self.running_since =
                (event.status == AgentAnalysisStageStatus::Running).then(Instant::now);
        }
        self.status = event.status;
        self.elapsed_seconds = event.elapsed_seconds;
        self.result_summary = event.result_summary;
        if event.status != AgentAnalysisStageStatus::Running {
            self.running_since = None;
        }
    }

    /// 在会话终止时关闭仍在旋转的阶段，并保存截至终止时的耗时。
    fn finish_running(&mut self, status: AgentAnalysisStageStatus) {
        if self.status != AgentAnalysisStageStatus::Running {
            return;
        }
        if let Some(started_at) = self.running_since.take() {
            self.elapsed_seconds = self
                .elapsed_seconds
                .saturating_add(started_at.elapsed().as_secs());
        }
        self.status = status;
        if self.result_summary.is_none() {
            self.result_summary = Some(
                match status {
                    AgentAnalysisStageStatus::Failed => "阶段因不可恢复错误中止",
                    AgentAnalysisStageStatus::Cancelled => "阶段已由用户主动取消",
                    _ => self.stage.default_result_summary(),
                }
                .to_string(),
            );
        }
    }

    /// 在收到更晚阶段或成功终态时补齐可能因事件通道背压遗漏的完成状态。
    fn complete_if_unfinished(&mut self) {
        match self.status {
            AgentAnalysisStageStatus::Pending => {
                self.status = AgentAnalysisStageStatus::Completed;
                self.result_summary = Some(self.stage.default_result_summary().to_string());
            }
            AgentAnalysisStageStatus::Running => {
                self.finish_running(AgentAnalysisStageStatus::Completed);
            }
            AgentAnalysisStageStatus::Completed
            | AgentAnalysisStageStatus::Failed
            | AgentAnalysisStageStatus::Cancelled => {}
        }
    }
}

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
    /// 固定分析流程的右侧悬浮时间线状态，只保留标题、结果摘要与耗时。
    analysis_stages: Vec<AgentStageViewState>,
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
    /// 最终结构化报告。
    report: Option<Arc<DiagnosticReport>>,
    /// 当前已经允许渲染的报告 Unicode 字符数量，用于分块流式展示。
    report_revealed_chars: usize,
    /// 当前报告动态文本总字符数，收到报告时一次计算，避免每帧重复遍历全文。
    report_stream_total_chars: usize,
    /// 报告虚拟行字符数：问题、无发现说明、逐发现、结论各占一项。
    report_stream_row_characters: Arc<Vec<usize>>,
    /// 每次收到新报告时递增，用于终止旧报告仍在等待的流式刷新任务。
    report_stream_generation: u64,
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
                            window.apply_event(event, cx);
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
            analysis_stages: AgentAnalysisStage::ALL
                .iter()
                .copied()
                .map(AgentStageViewState::pending)
                .collect(),
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
            report: None,
            report_revealed_chars: 0,
            report_stream_total_chars: 0,
            report_stream_row_characters: Arc::new(Vec::new()),
            report_stream_generation: 0,
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
    fn apply_event(&mut self, event: AgentEvent, cx: &mut Context<Self>) {
        match event {
            AgentEvent::Status(status) => {
                self.status = status;
                let terminal_stage_status = match status {
                    AgentSessionStatus::Completed => Some(AgentAnalysisStageStatus::Completed),
                    AgentSessionStatus::Cancelled => Some(AgentAnalysisStageStatus::Cancelled),
                    AgentSessionStatus::Failed => Some(AgentAnalysisStageStatus::Failed),
                    _ => None,
                };
                if let Some(stage_status) = terminal_stage_status {
                    for stage in &mut self.analysis_stages {
                        if stage_status == AgentAnalysisStageStatus::Completed {
                            stage.complete_if_unfinished();
                        } else {
                            stage.finish_running(stage_status);
                        }
                    }
                }
                self.push_trace(AgentTraceEntry::new(
                    AgentTraceKind::Status,
                    format!("状态：{}", status.label()),
                    "会话状态已更新",
                ));
            }
            AgentEvent::Trace(trace) => self.push_trace(trace),
            AgentEvent::Budget(budget) => self.budget = budget,
            AgentEvent::Stage(event) => {
                // 阶段事件单调递增；若有中间事件因有界通道拥塞被丢弃，较晚事件可以确定此前阶段
                // 已经完成，因此先补齐前缀状态，避免时间线在成功会话中永久显示“待开始”。
                if event.status != AgentAnalysisStageStatus::Pending {
                    for stage in self.analysis_stages.iter_mut().take(event.stage.index()) {
                        stage.complete_if_unfinished();
                    }
                }
                if let Some(stage) = self
                    .analysis_stages
                    .iter_mut()
                    .find(|stage| stage.stage == event.stage)
                {
                    stage.apply(event);
                }
            }
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
                    "分析报告生成中",
                    "正在整理问题描述、问题分析、结论及建议",
                ));
                self.start_report_stream(report, report_path, cx);
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

    /// 启动结构化报告的分块流式展示任务。
    fn start_report_stream(
        &mut self,
        report: DiagnosticReport,
        report_path: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.report_stream_row_characters =
            Arc::new(report_stream_row_character_counts(&self.question, &report));
        self.report_stream_total_chars = self.report_stream_row_characters.iter().sum();
        self.report = Some(Arc::new(report));
        self.report_path = report_path;
        self.report_revealed_chars = 0;
        self.report_stream_generation = self.report_stream_generation.wrapping_add(1);
        let generation = self.report_stream_generation;

        cx.spawn(async move |view, cx| {
            loop {
                Timer::after(REPORT_STREAM_INTERVAL).await;
                let update_result = view.update(cx, |window, cx| {
                    if window.report_stream_generation != generation {
                        return true;
                    }
                    let is_complete = window.advance_report_stream();
                    window.sync_trace_items();
                    cx.notify();
                    is_complete
                });
                match update_result {
                    Ok(true) | Err(_) => break,
                    Ok(false) => {}
                }
            }
        })
        .detach();
    }

    /// 推进一次报告流并返回内容是否已经完整显示。
    fn advance_report_stream(&mut self) -> bool {
        if self.report.is_none() {
            return true;
        }
        self.report_revealed_chars = self
            .report_revealed_chars
            .saturating_add(REPORT_STREAM_CHARS_PER_TICK)
            .min(self.report_stream_total_chars);
        self.report_revealed_chars >= self.report_stream_total_chars
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

    /// 根据当前轨迹、报告流进度和展开状态更新虚拟列表条目，并只失效变化的连续区间。
    fn sync_trace_items(&mut self) {
        let next_items = build_agent_stream_items(
            &self.traces,
            self.status,
            self.report.as_deref(),
            self.report_revealed_chars,
            self.report_stream_total_chars,
            &self.report_stream_row_characters,
            &self.expanded_tool_groups,
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

/// 构建当前消息流的轻量虚拟条目，不复制轨迹正文或报告内容。
fn build_agent_stream_items(
    traces: &[Arc<AgentTraceEntry>],
    status: AgentSessionStatus,
    report: Option<&DiagnosticReport>,
    report_revealed_chars: usize,
    report_total_chars: usize,
    report_row_characters: &[usize],
    expanded_tool_groups: &HashSet<i64>,
) -> Vec<AgentStreamItem> {
    let mut items = Vec::with_capacity(traces.len().saturating_add(6));
    items.push(AgentStreamItem::Question);
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

    let mut trace_index = 0;
    while trace_index < traces.len() {
        let trace = &traces[trace_index];
        // 最终报告由拆分后的虚拟卡片行展示，隐藏仅用于状态提示的报告轨迹。
        if trace.kind == AgentTraceKind::Report && report.is_some() {
            trace_index += 1;
            continue;
        }
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

    if let Some(report) = report {
        append_report_stream_items(
            &mut items,
            report,
            report_revealed_chars,
            report_total_chars,
            report_row_characters,
        );
    } else {
        items.push(AgentStreamItem::Spacer);
    }
    items
}

/// 把报告拆成顶部、分析标题、逐发现和结论行，使长报告也只布局当前可见部分。
fn append_report_stream_items(
    items: &mut Vec<AgentStreamItem>,
    report: &DiagnosticReport,
    revealed_chars: usize,
    total_chars: usize,
    row_characters: &[usize],
) {
    let is_complete = revealed_chars >= total_chars;
    let mut row_start = 0usize;
    let question_length = row_characters.first().copied().unwrap_or_default();
    items.push(AgentStreamItem::ReportHeader {
        visible_chars: visible_chars_in_report_row(revealed_chars, row_start, question_length),
        is_complete,
    });
    row_start = row_start.saturating_add(question_length);

    let empty_message_length = row_characters.get(1).copied().unwrap_or_default();
    items.push(AgentStreamItem::ReportAnalysisHeader {
        visible_empty_message_chars: visible_chars_in_report_row(
            revealed_chars,
            row_start,
            empty_message_length,
        ),
        is_visible: revealed_chars >= row_start,
    });
    row_start = row_start.saturating_add(empty_message_length);

    for (finding_index, finding) in report.findings.iter().enumerate() {
        let row_length = row_characters
            .get(finding_index.saturating_add(2))
            .copied()
            .unwrap_or_else(|| finding_analysis_character_count(finding));
        items.push(AgentStreamItem::ReportFinding {
            finding_index,
            visible_chars: visible_chars_in_report_row(revealed_chars, row_start, row_length),
        });
        row_start = row_start.saturating_add(row_length);
    }

    let footer_length = row_characters
        .last()
        .copied()
        .unwrap_or_else(|| report_footer_character_count(report));
    items.push(AgentStreamItem::ReportFooter {
        visible_chars: visible_chars_in_report_row(revealed_chars, row_start, footer_length),
        is_complete,
        is_visible: revealed_chars > row_start || (footer_length == 0 && is_complete),
    });
    // 变量仅用于在调试构建中校验拆分计数保持一致，避免流式报告永远无法完成。
    debug_assert_eq!(
        row_start.saturating_add(footer_length),
        total_chars,
        "报告虚拟行字符计数与总字符数不一致"
    );
}

/// 计算某个报告虚拟行在当前全局流式进度下可见的字符数。
fn visible_chars_in_report_row(
    revealed_chars: usize,
    row_start: usize,
    row_length: usize,
) -> usize {
    revealed_chars.saturating_sub(row_start).min(row_length)
}

/// 找出新旧虚拟条目的变化区间；等长更新按离散区间失效，避免报告完成态波及中间内容。
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
                    .overflow_hidden()
                    .child(
                        div()
                            .relative()
                            .size_full()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .child(render_budget_bar(
                                self.budget,
                                self.context_window_tokens,
                                status,
                                &theme,
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_h(px(0.0))
                                    .mr(px(
                                        AGENT_STAGE_CARD_WIDTH + AGENT_STAGE_CARD_GAP * 2.0,
                                    ))
                                    .overflow_hidden()
                                    .child(render_message_stream(
                                        &self.question,
                                        self.traces.clone(),
                                        self.trace_items.clone(),
                                        self.report.clone(),
                                        self.report_path.as_deref(),
                                        self.app.clone(),
                                        self.scope.clone(),
                                        self.trace_list.clone(),
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
                                        .right(px(
                                            AGENT_STAGE_CARD_WIDTH + AGENT_STAGE_CARD_GAP * 2.0,
                                        ))
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
                                    .right(px(
                                        AGENT_STAGE_CARD_WIDTH + AGENT_STAGE_CARD_GAP * 2.0,
                                    ))
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
                                            )
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
                            ),
                    )
                    .child(render_analysis_stage_timeline_card(
                        &self.analysis_stages,
                        &theme,
                    )),
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

/// 渲染窗口右侧悬浮的固定分析流程时间线卡片。
///
/// 卡片与窗口边缘保持间距，不参与主布局分栏；内容严格限制为阶段标题、结果摘要和耗时。
/// 模型思考、工具参数及证据正文继续只在消息瀑布流展示。
fn render_analysis_stage_timeline_card(
    stages: &[AgentStageViewState],
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let completed_count = stages
        .iter()
        .filter(|stage| stage.status == AgentAnalysisStageStatus::Completed)
        .count();
    div()
        .absolute()
        .top(px(AGENT_STAGE_CARD_TOP))
        .right(px(AGENT_STAGE_CARD_GAP))
        .bottom(px(AGENT_STAGE_CARD_GAP))
        .w(px(AGENT_STAGE_CARD_WIDTH))
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded_lg()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.content))
        .shadow_lg()
        .child(
            div()
                .h(px(42.0))
                .px_3()
                .flex_none()
                .flex()
                .items_center()
                .justify_between()
                .border_b_1()
                .border_color(rgb(theme.border))
                .child(
                    div()
                        .text_size(px(12.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("分析进度"),
                )
                .child(
                    div()
                        .text_size(px(9.0))
                        .text_color(rgb(theme.foreground_muted))
                        .child(format!("{completed_count} / {} 已完成", stages.len())),
                ),
        )
        .child(div().flex_1().min_h(px(0.0)).overflow_hidden().children(
            stages.iter().enumerate().map(|(index, stage)| {
                render_analysis_stage_timeline_item(index, stages.len(), stage, theme)
            }),
        ))
}

/// 渲染单个紧凑时间线节点，完成节点同时展示阶段结果摘要与耗时。
fn render_analysis_stage_timeline_item(
    index: usize,
    stage_count: usize,
    stage: &AgentStageViewState,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    let (summary, duration, color) = analysis_stage_display(stage, theme);
    let node: AnyElement = match stage.status {
        AgentAnalysisStageStatus::Running => render_loading_spinner(
            ("agent-analysis-stage-loading", stage.stage.index()),
            theme.info,
            12.0,
        ),
        AgentAnalysisStageStatus::Pending => div()
            .size(px(7.0))
            .rounded_full()
            .border_1()
            .border_color(rgb(theme.foreground_muted))
            .bg(rgb(theme.content))
            .into_any_element(),
        _ => div()
            .size(px(7.0))
            .rounded_full()
            .bg(rgb(color))
            .into_any_element(),
    };
    let line_color = if stage.status == AgentAnalysisStageStatus::Completed {
        theme.success
    } else {
        theme.border
    };
    div()
        .h(px(36.0))
        .px_3()
        .flex_none()
        .flex()
        .gap_2()
        .child(
            div()
                .relative()
                .w(px(14.0))
                .h_full()
                .flex_none()
                .when(index + 1 < stage_count, |this| {
                    this.child(
                        div()
                            .absolute()
                            .left(px(6.0))
                            .top(px(18.0))
                            .bottom(px(-18.0))
                            .w(px(1.0))
                            .bg(rgb(line_color)),
                    )
                })
                .child(
                    div()
                        .absolute()
                        .top(px(5.0))
                        .left(px(0.5))
                        .size(px(12.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(node),
                ),
        )
        .child(
            div()
                .min_w(px(0.0))
                .flex_1()
                .pt(px(2.0))
                .child(
                    div()
                        .truncate()
                        .text_size(px(10.5))
                        .font_weight(if stage.status == AgentAnalysisStageStatus::Running {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        })
                        .text_color(rgb(if stage.status == AgentAnalysisStageStatus::Running {
                            theme.foreground
                        } else {
                            theme.foreground_muted
                        }))
                        .child(stage.stage.title()),
                )
                .child(
                    div()
                        .mt(px(1.0))
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .min_w(px(0.0))
                                .flex_1()
                                .truncate()
                                .text_size(px(9.0))
                                .text_color(rgb(color))
                                .child(summary),
                        )
                        .when_some(duration, |this, duration| {
                            this.child(
                                div()
                                    .flex_none()
                                    .text_size(px(8.5))
                                    .text_color(rgb(theme.syntax.comment))
                                    .child(duration),
                            )
                        }),
                ),
        )
}

/// 返回时间线节点的结果摘要、可选耗时与语义色。
fn analysis_stage_display(
    stage: &AgentStageViewState,
    theme: &AppTheme,
) -> (String, Option<String>, u32) {
    match stage.status {
        AgentAnalysisStageStatus::Pending => ("待开始".to_string(), None, theme.foreground_muted),
        AgentAnalysisStageStatus::Running => ("正在执行".to_string(), None, theme.info),
        AgentAnalysisStageStatus::Completed => (
            stage
                .result_summary
                .clone()
                .unwrap_or_else(|| stage.stage.default_result_summary().to_string()),
            Some(format_stage_duration(stage.elapsed_seconds)),
            theme.success,
        ),
        AgentAnalysisStageStatus::Failed => (
            stage
                .result_summary
                .clone()
                .unwrap_or_else(|| "阶段因错误中止".to_string()),
            Some(format_stage_duration(stage.elapsed_seconds)),
            theme.error,
        ),
        AgentAnalysisStageStatus::Cancelled => (
            stage
                .result_summary
                .clone()
                .unwrap_or_else(|| "阶段已由用户取消".to_string()),
            Some(format_stage_duration(stage.elapsed_seconds)),
            theme.warning,
        ),
    }
}

/// 把阶段秒数压缩为适合悬浮时间线的一行耗时文本。
fn format_stage_duration(seconds: u64) -> String {
    match seconds {
        0 => "< 1 秒".to_string(),
        1..=59 => format!("{seconds} 秒"),
        _ => format!("{} 分 {} 秒", seconds / 60, seconds % 60),
    }
}

/// 渲染资源预算条。
fn render_budget_bar(
    budget: AgentBudgetSnapshot,
    context_window_tokens: u64,
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
        .child(div().w(px(78.0)).flex_none().child(render_budget_metric(
            format!("耗时 {}s", budget.elapsed_seconds),
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

/// 渲染单列消息瀑布流，并把最终报告作为同一消息流中的最后一条 Agent 消息。
#[allow(clippy::too_many_arguments)]
fn render_message_stream(
    question: &str,
    traces: Arc<Vec<Arc<AgentTraceEntry>>>,
    items: Vec<AgentStreamItem>,
    report: Option<Arc<DiagnosticReport>>,
    report_path: Option<&str>,
    app: Entity<ArgusApp>,
    scope: Arc<SourceScopeSnapshot>,
    list_state: ListState,
    agent_window: Entity<AgentWindow>,
    theme: &AppTheme,
) -> impl IntoElement {
    let question = Arc::<str>::from(question);
    let report_path = report_path.map(Arc::<str>::from);
    let items = Arc::new(items);
    let render_items = items.clone();
    let render_traces = traces.clone();
    let render_report = report.clone();
    let render_app = app.clone();
    let render_scope = scope.clone();
    let render_agent_window = agent_window.clone();
    let render_theme = theme.clone();
    let render_question = question.clone();
    let render_report_path = report_path.clone();

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
                    render_report.as_deref(),
                    render_report_path.as_deref(),
                    render_app.clone(),
                    render_scope.clone(),
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
#[allow(clippy::too_many_arguments)]
fn render_agent_stream_item(
    item: &AgentStreamItem,
    question: &str,
    traces: &[Arc<AgentTraceEntry>],
    report: Option<&DiagnosticReport>,
    report_path: Option<&str>,
    app: Entity<ArgusApp>,
    scope: Arc<SourceScopeSnapshot>,
    agent_window: Entity<AgentWindow>,
    theme: &AppTheme,
) -> AnyElement {
    match *item {
        AgentStreamItem::Question => render_question_message(question, theme).into_any_element(),
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
        AgentStreamItem::ReportHeader {
            visible_chars,
            is_complete,
        } => render_report_header(question, visible_chars, is_complete, theme).into_any_element(),
        AgentStreamItem::ReportAnalysisHeader {
            visible_empty_message_chars,
            is_visible,
        } => render_report_analysis_header(visible_empty_message_chars, is_visible, theme)
            .into_any_element(),
        AgentStreamItem::ReportFinding {
            finding_index,
            visible_chars,
        } => report
            .and_then(|report| report.findings.get(finding_index))
            .map_or_else(
                || div().h(px(0.0)).into_any_element(),
                |finding| {
                    render_report_finding(finding, finding_index, visible_chars, app, scope, theme)
                        .into_any_element()
                },
            ),
        AgentStreamItem::ReportFooter {
            visible_chars,
            is_complete,
            is_visible,
        } => report.map_or_else(
            || div().h(px(0.0)).into_any_element(),
            |report| {
                render_report_footer(
                    report,
                    report_path,
                    visible_chars,
                    is_complete,
                    is_visible,
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
        AgentTraceKind::Reasoning
            | AgentTraceKind::Output
            | AgentTraceKind::User
            | AgentTraceKind::Report
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

/// 报告流式文本游标；每次渲染按固定顺序消费可见字符预算。
struct ReportStreamCursor {
    /// 本次渲染尚可展示的 Unicode 字符数量。
    remaining_chars: usize,
}

impl ReportStreamCursor {
    /// 返回当前字段可见的字符前缀；前序字段未完成时返回 `None`。
    fn take(&mut self, value: &str) -> Option<String> {
        if value.is_empty() || self.remaining_chars == 0 {
            return None;
        }
        let character_count = value.chars().count();
        let visible_count = character_count.min(self.remaining_chars);
        self.remaining_chars = self.remaining_chars.saturating_sub(visible_count);
        Some(value.chars().take(visible_count).collect())
    }
}

/// 一次计算报告各虚拟行的动态字符数，后续流式帧只做常数时间的区间换算。
fn report_stream_row_character_counts(question: &str, report: &DiagnosticReport) -> Vec<usize> {
    let mut row_characters = Vec::with_capacity(report.findings.len().saturating_add(3));
    row_characters.push(question.chars().count());
    row_characters.push(if report.findings.is_empty() {
        EMPTY_FINDINGS_MESSAGE.chars().count()
    } else {
        0
    });
    for finding in &report.findings {
        row_characters.push(finding_analysis_character_count(finding));
    }
    row_characters.push(report_footer_character_count(report));
    row_characters
}

/// 计算一条问题发现中分析、影响和证据片段的流式字符数。
fn finding_analysis_character_count(finding: &DiagnosticFinding) -> usize {
    let mut count = finding
        .title
        .chars()
        .count()
        .saturating_add(finding.analysis.chars().count())
        .saturating_add(finding.impact.chars().count());
    for evidence in &finding.evidence {
        count = count.saturating_add(evidence.rationale.chars().count());
        if let Some(excerpt) = &evidence.display_excerpt {
            for line in &excerpt.lines {
                count = count.saturating_add(line.text.chars().count());
            }
        }
    }
    count
}

/// 计算结论、建议、验证步骤和限制说明的流式字符数。
fn report_footer_character_count(report: &DiagnosticReport) -> usize {
    let mut count = report.summary.chars().count();
    for finding in &report.findings {
        count = count.saturating_add(finding.recommendation.chars().count());
        for step in &finding.verification_steps {
            count = count.saturating_add(step.chars().count());
        }
    }
    for limitation in &report.limitations {
        count = count.saturating_add(limitation.chars().count());
    }
    count
}

/// 渲染报告内稳定的编号分区标题。
fn render_report_section_header(
    index: &'static str,
    title: &'static str,
    theme: &AppTheme,
) -> impl IntoElement + use<> {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .size(px(20.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(rgb(theme.selection))
                .text_size(px(10.0))
                .font_weight(FontWeight::SEMIBOLD)
                .child(index),
        )
        .child(
            div()
                .text_size(px(13.0))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
}

/// 渲染报告卡片顶部和问题描述；后续虚拟行沿用相同边框与背景形成一张连续大卡片。
fn render_report_header(
    question: &str,
    visible_chars: usize,
    is_stream_complete: bool,
    theme: &AppTheme,
) -> impl IntoElement {
    let mut cursor = ReportStreamCursor {
        remaining_chars: visible_chars,
    };
    let visible_question = cursor.take(question);
    let report_leading = if is_stream_complete {
        render_icon(ArgusIcon::FileText, theme.info, 16.0).into_any_element()
    } else {
        render_loading_spinner(("agent-report-stream-loading", 0), theme.info, 16.0)
    };

    div().w_full().px_6().pt_3().flex().justify_center().child(
        div()
            .w_full()
            .max_w(px(AGENT_STREAM_MAX_WIDTH))
            .px_5()
            .pt_5()
            .border_t_1()
            .border_l_1()
            .border_r_1()
            .border_color(rgb(theme.border))
            .rounded_t(px(8.0))
            .bg(rgb(theme.content))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pb_4()
                    .border_b_1()
                    .border_color(rgb(theme.border))
                    .child(report_leading)
                    .child(
                        div()
                            .text_size(px(14.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("智能分析报告"),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(rgb(theme.foreground_muted))
                            .child(if is_stream_complete {
                                "已完成"
                            } else {
                                "正在生成"
                            }),
                    ),
            )
            .child(
                div()
                    .pt_4()
                    .child(render_report_section_header("1", "问题描述", theme))
                    .when_some(visible_question, |this, question| {
                        this.child(
                            div()
                                .mt_3()
                                .text_size(px(12.0))
                                .line_height(px(20.0))
                                .child(question),
                        )
                    }),
            ),
    )
}

/// 渲染问题分析分区标题；报告无发现时在同一虚拟行展示保守说明。
fn render_report_analysis_header(
    visible_empty_message_chars: usize,
    is_visible: bool,
    theme: &AppTheme,
) -> impl IntoElement {
    if !is_visible {
        return div().h(px(0.0));
    }
    let visible_empty_message = if visible_empty_message_chars == 0 {
        None
    } else {
        Some(
            EMPTY_FINDINGS_MESSAGE
                .chars()
                .take(visible_empty_message_chars)
                .collect::<String>(),
        )
    };
    div().w_full().px_6().flex().justify_center().child(
        div()
            .w_full()
            .max_w(px(AGENT_STREAM_MAX_WIDTH))
            .px_5()
            .pt_5()
            .border_l_1()
            .border_r_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.content))
            .child(
                div()
                    .pt_5()
                    .border_t_1()
                    .border_color(rgb(theme.border))
                    .child(render_report_section_header("2", "问题分析", theme))
                    .when_some(visible_empty_message, |this, message| {
                        this.child(
                            div()
                                .mt_3()
                                .text_size(px(12.0))
                                .line_height(px(20.0))
                                .text_color(rgb(theme.foreground_muted))
                                .child(message),
                        )
                    }),
            ),
    )
}

/// 渲染一条报告发现；尚未流式展示到该发现时返回零高度行。
#[allow(clippy::too_many_arguments)]
fn render_report_finding(
    finding: &DiagnosticFinding,
    finding_index: usize,
    visible_chars: usize,
    app: Entity<ArgusApp>,
    scope: Arc<SourceScopeSnapshot>,
    theme: &AppTheme,
) -> impl IntoElement {
    if visible_chars == 0 {
        return div().h(px(0.0));
    }
    let mut cursor = ReportStreamCursor {
        remaining_chars: visible_chars,
    };
    let visible_title = cursor.take(&finding.title);
    let visible_analysis = cursor.take(&finding.analysis);
    let visible_impact = cursor.take(&finding.impact);
    let mut evidence_elements = Vec::<AnyElement>::new();
    for (evidence_index, evidence) in finding.evidence.iter().enumerate() {
        let visible_rationale = cursor.take(&evidence.rationale);
        let mut excerpt_lines = Vec::<AnyElement>::new();
        if let Some(excerpt) = &evidence.display_excerpt {
            for line in &excerpt.lines {
                if let Some(text) = cursor.take(&line.text) {
                    excerpt_lines.push(
                        div()
                            .flex()
                            .items_start()
                            .gap_3()
                            .child(
                                div()
                                    .w(px(42.0))
                                    .flex_none()
                                    .text_right()
                                    .text_color(rgb(theme.syntax.comment))
                                    .child(line.line_number.to_string()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .whitespace_normal()
                                    .child(text),
                            )
                            .into_any_element(),
                    );
                }
            }
        }
        let Some(rationale) = visible_rationale else {
            continue;
        };
        let source = scope.source(&evidence.source_ref);
        let source_id = source.map(|source| source.source_id);
        let label = source
            .map(|source| source.relative_path.clone())
            .unwrap_or_else(|| "来源已失效".to_string());
        let start_line = evidence.start_line;
        let navigate_app = app.clone();
        let has_excerpt_lines = !excerpt_lines.is_empty();
        let excerpt_is_truncated = evidence
            .display_excerpt
            .as_ref()
            .is_some_and(|excerpt| excerpt.is_truncated);
        evidence_elements.push(
            div()
                .id((
                    "agent-report-evidence",
                    finding_index * 1000 + evidence_index,
                ))
                .mt_3()
                .child(
                    div()
                        .id((
                            "agent-report-evidence-link",
                            finding_index * 1000 + evidence_index,
                        ))
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
                            label, evidence.start_line, evidence.end_line, rationale
                        )),
                )
                .when(has_excerpt_lines, |this| {
                    this.child(
                        div()
                            .mt_2()
                            .py_2()
                            .pr_3()
                            .border_l_1()
                            .border_color(rgb(theme.info))
                            .bg(rgb(theme.current_line))
                            .font_family(ARGUS_LOG_FONT_FAMILY)
                            .text_size(px(10.0))
                            .line_height(px(17.0))
                            .children(excerpt_lines)
                            .when(excerpt_is_truncated, |excerpt| {
                                excerpt.child(
                                    div()
                                        .ml(px(54.0))
                                        .text_color(rgb(theme.syntax.comment))
                                        .child("… 片段已按展示边界截断"),
                                )
                            }),
                    )
                })
                .into_any_element(),
        );
    }

    div().w_full().px_6().flex().justify_center().child(
        div()
            .w_full()
            .max_w(px(AGENT_STREAM_MAX_WIDTH))
            .px_5()
            .border_l_1()
            .border_r_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.content))
            .child(
                div()
                    .pt_4()
                    .border_t_1()
                    .border_color(rgb(theme.border))
                    .when_some(visible_title, |this, title| {
                        this.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.0))
                                        .text_color(rgb(theme.foreground_muted))
                                        .child(format!(
                                            "{} · {} · 置信度 {:.0}%",
                                            finding.severity,
                                            finding.status.label(),
                                            finding.confidence * 100.0
                                        )),
                                ),
                        )
                    })
                    .when_some(visible_analysis, |this, analysis| {
                        this.child(
                            div()
                                .mt_2()
                                .text_size(px(12.0))
                                .line_height(px(20.0))
                                .child(analysis),
                        )
                    })
                    .when_some(visible_impact, |this, impact| {
                        this.child(
                            div()
                                .mt_2()
                                .text_size(px(11.0))
                                .line_height(px(18.0))
                                .text_color(rgb(theme.foreground_muted))
                                .child(format!("影响：{impact}")),
                        )
                    })
                    .children(evidence_elements),
            ),
    )
}

/// 渲染报告结论、建议和限制，并以底部圆角结束连续卡片。
fn render_report_footer(
    report: &DiagnosticReport,
    report_path: Option<&str>,
    visible_chars: usize,
    is_stream_complete: bool,
    is_visible: bool,
    theme: &AppTheme,
) -> impl IntoElement {
    if !is_visible {
        return div().h(px(0.0));
    }
    let mut cursor = ReportStreamCursor {
        remaining_chars: visible_chars,
    };
    let visible_summary = cursor.take(&report.summary);
    let mut recommendation_elements = Vec::<AnyElement>::new();
    for (index, finding) in report.findings.iter().enumerate() {
        if let Some(recommendation) = cursor.take(&finding.recommendation) {
            recommendation_elements.push(
                div()
                    .mt_2()
                    .flex()
                    .items_start()
                    .gap_2()
                    .text_size(px(11.0))
                    .line_height(px(18.0))
                    .child(
                        div()
                            .flex_none()
                            .text_color(rgb(theme.info))
                            .child(format!("建议 {}", index + 1)),
                    )
                    .child(div().flex_1().min_w(px(0.0)).child(recommendation))
                    .into_any_element(),
            );
        }
        for step in &finding.verification_steps {
            if let Some(verification) = cursor.take(step) {
                recommendation_elements.push(
                    div()
                        .mt_2()
                        .ml_3()
                        .text_size(px(10.0))
                        .line_height(px(17.0))
                        .text_color(rgb(theme.foreground_muted))
                        .child(format!("待验证：{verification}"))
                        .into_any_element(),
                );
            }
        }
    }
    let mut limitation_elements = Vec::<AnyElement>::new();
    for limitation in &report.limitations {
        if let Some(text) = cursor.take(limitation) {
            limitation_elements.push(
                div()
                    .mt_2()
                    .text_size(px(10.0))
                    .line_height(px(17.0))
                    .text_color(rgb(theme.warning))
                    .child(format!("限制：{text}"))
                    .into_any_element(),
            );
        }
    }
    div().w_full().px_6().pb_8().flex().justify_center().child(
        div()
            .w_full()
            .max_w(px(AGENT_STREAM_MAX_WIDTH))
            .px_5()
            .pb_5()
            .border_l_1()
            .border_r_1()
            .border_b_1()
            .border_color(rgb(theme.border))
            .rounded_b(px(8.0))
            .bg(rgb(theme.content))
            .child(
                div()
                    .pt_5()
                    .border_t_1()
                    .border_color(rgb(theme.border))
                    .child(render_report_section_header("3", "结论及建议", theme))
                    .when_some(visible_summary, |this, summary| {
                        this.child(
                            div()
                                .mt_3()
                                .text_size(px(12.0))
                                .line_height(px(20.0))
                                .child(summary),
                        )
                    })
                    .children(recommendation_elements)
                    .children(limitation_elements)
                    .when(is_stream_complete, |this| {
                        this.when_some(report_path.map(str::to_string), |section, path| {
                            section.child(
                                div()
                                    .mt_4()
                                    .text_size(px(10.0))
                                    .text_color(rgb(theme.syntax.comment))
                                    .child(format!("报告已保存：{path}")),
                            )
                        })
                    }),
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

    /// 验证报告游标按 Unicode 字符而不是 UTF-8 字节逐步展示，避免截断中文字符。
    #[test]
    fn report_stream_cursor_reveals_unicode_prefix() {
        let mut cursor = ReportStreamCursor { remaining_chars: 3 };
        assert_eq!(cursor.take("内存异常"), Some("内存异".to_string()));
        assert_eq!(cursor.take("后续字段"), None);
    }

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

    /// 验证报告字符数只在接收报告时计算一次，并且各虚拟行之和覆盖完整流式内容。
    #[test]
    fn report_stream_rows_cover_all_dynamic_text() {
        let report = DiagnosticReport {
            session_id: "session".to_string(),
            question_sha256: "0".repeat(64),
            summary: "结论摘要".to_string(),
            findings: Vec::new(),
            used_log_profiles: Vec::new(),
            limitations: vec!["样本范围有限".to_string()],
            completed_at: "2026-07-16T00:00:00Z".to_string(),
        };
        let rows = report_stream_row_character_counts("内存问题", &report);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], "内存问题".chars().count());
        assert_eq!(rows[1], EMPTY_FINDINGS_MESSAGE.chars().count());
        assert_eq!(
            rows[2],
            report.summary.chars().count() + report.limitations[0].chars().count()
        );
        assert_eq!(
            visible_chars_in_report_row(rows[0] + 2, rows[0], rows[1]),
            2
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

    /// 验证阶段视图接收运行与完成快照后保存结果和耗时，终态不会留下加载状态。
    #[test]
    fn analysis_stage_view_applies_structured_results() {
        let mut stage = AgentStageViewState::pending(AgentAnalysisStage::ExtractContext);
        stage.apply(AgentAnalysisStageEvent {
            stage: AgentAnalysisStage::ExtractContext,
            status: AgentAnalysisStageStatus::Running,
            elapsed_seconds: 2,
            result_summary: None,
        });
        assert_eq!(stage.status, AgentAnalysisStageStatus::Running);
        assert!(stage.running_since.is_some());

        stage.apply(AgentAnalysisStageEvent {
            stage: AgentAnalysisStage::ExtractContext,
            status: AgentAnalysisStageStatus::Completed,
            elapsed_seconds: 7,
            result_summary: Some("已定位启动失败上下文".to_string()),
        });
        assert_eq!(stage.status, AgentAnalysisStageStatus::Completed);
        assert_eq!(stage.elapsed_seconds, 7);
        assert_eq!(
            stage.result_summary.as_deref(),
            Some("已定位启动失败上下文")
        );
        assert!(stage.running_since.is_none());
        assert_eq!(format_stage_duration(7), "7 秒");
    }
}
