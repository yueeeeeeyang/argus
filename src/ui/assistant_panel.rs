//! 文件职责：渲染并管理主窗口右侧可持续交互的 Agent 助手面板。
//! 创建日期：2026-07-16
//! 修改日期：2026-07-16
//! 作者：Argus 开发团队
//! 主要功能：全来源扫描、多轮会话、模型切换、流式 Markdown、工具轨迹分组、Token 状态和可信日志引用跳转。

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, FontWeight, IntoElement, KeyDownEvent,
    ListAlignment, ListState, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Render, ScrollHandle, Subscription, Timer, Window, canvas, div, list, point, prelude::*, px,
    rgb,
};

use crate::agent::report::AssistantCitation;
use crate::agent::{
    AgentBudgetSnapshot, AgentEvent, AgentScopeSelection, AgentSessionStatus, AgentStreamKind,
    AgentTraceEntry, AgentTraceKind, AgentUserMessage, AgentUserMessageStatus,
    AssistantHistoryTurn, AssistantRunRequest, SourceScopeSnapshot, agent_runtime, load_api_key,
    prepare_agent_source_scope_for_selection, run_assistant_turn,
};
use crate::app::{ArgusApp, TextInputState, observe_app_theme};
use crate::config::{AiConfig, AiModelProfile};
use crate::fonts::{ARGUS_LOG_FONT_FAMILY, ARGUS_UI_FONT_FAMILY};
use crate::infra::text_selection::{character_count, replace_character_range};
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_round_icon_button};
use crate::ui::components::input::{
    InputAccessory, InputPointerAction, InputPointerEvent, NativeInput, Textarea,
    TextareaAccessoryPosition, TextareaScrollState, TextareaStyle, render_textarea,
};
use crate::ui::components::input_behavior::{LocalInputAction, handle_local_input_key};
use crate::ui::components::loading_spinner::render_loading_spinner;
use crate::ui::components::markdown::{MarkdownStyle, render_markdown};
use crate::ui::components::scrollbar::{scrollbar_metrics, scrollbar_scroll_for_drag};

/// 面板流式事件合并窗口，限制高吞吐模型触发的重绘频率。
const ASSISTANT_EVENT_BATCH_INTERVAL: Duration = Duration::from_millis(16);
/// 单条用户消息边界；避免异常输入无限扩张内存和模型请求。
const ASSISTANT_MESSAGE_MAX_BYTES: usize = 32 * 1024;
/// 虚拟消息列表上下预渲染高度。
const ASSISTANT_LIST_OVERDRAW: f32 = 240.0;
/// 消息滚动条滑块宽度，不绘制轨道。
const ASSISTANT_SCROLLBAR_WIDTH: f32 = 4.0;
/// 消息滚动条上下留白。
const ASSISTANT_SCROLLBAR_PADDING: f32 = 4.0;
/// 消息滚动条最小滑块高度。
const ASSISTANT_SCROLLBAR_MIN_THUMB: f32 = 24.0;
/// “@” 来源候选单次展示数量；限制布局高度并避免大来源树造成大量 GPUI 元素。
const ASSISTANT_MENTION_RESULT_LIMIT: usize = 8;

/// 助手输入中可由用户通过“@”选择的来源类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AssistantMentionKind {
    /// 一个目录或归档目录，模型通过 `list_sources.path_prefix` 分页解析其日志后代。
    Folder,
    /// 一个可读取日志文件，直接绑定当前不可变快照中的 `source_ref`。
    File,
}

impl AssistantMentionKind {
    /// 返回发送给模型的稳定类型文本。
    fn as_str(self) -> &'static str {
        match self {
            Self::Folder => "folder",
            Self::File => "file",
        }
    }
}

/// 从不可变来源快照构建的安全“@”候选，不包含真实磁盘路径。
#[derive(Clone, Debug)]
struct AssistantMentionCandidate {
    /// 文件或目录在多根快照中的唯一展示路径。
    display_path: String,
    /// 预计算小写搜索文本，避免每次输入都重新分配。
    search_key: String,
    /// 候选类型。
    kind: AssistantMentionKind,
    /// 文件候选对应的不透明引用；目录候选通过路径前缀解析。
    source_ref: Option<String>,
    /// 目录内日志后代数量；文件固定为 1。
    matching_source_count: usize,
}

/// 当前输入已经明确选择的候选；只有对应标记仍存在于正文时才随消息发送。
#[derive(Clone, Debug)]
struct AssistantSelectedMention {
    /// 插入输入框的可见标记。
    marker: String,
    /// 受信候选元数据。
    candidate: AssistantMentionCandidate,
}

/// 光标处正在编辑的“@”查询及其 Unicode 字符范围。
#[derive(Clone, Debug, Eq, PartialEq)]
struct AssistantMentionQuery {
    /// 从 `@` 到当前光标的字符范围。
    range: Range<usize>,
    /// 不含 `@` 的搜索文本。
    text: String,
}

/// 右侧助手当前运行状态；每轮完成后回到空闲，面板和对话历史继续保留。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AssistantPanelStatus {
    Idle,
    Scanning,
    Running,
    Cancelling,
}

impl AssistantPanelStatus {
    /// 返回头部紧凑状态文案。
    fn label(self) -> &'static str {
        match self {
            Self::Idle => "就绪",
            Self::Scanning => "扫描来源",
            Self::Running => "回答中",
            Self::Cancelling => "正在停止",
        }
    }

    /// 返回当前是否存在可主动停止的后台工作。
    fn is_busy(self) -> bool {
        matches!(self, Self::Scanning | Self::Running | Self::Cancelling)
    }
}

/// 虚拟消息流中的稳定业务条目。
#[derive(Clone, Debug)]
enum AssistantPanelMessage {
    /// 用户输入；运行中补充会经历排队和消费状态变化。
    User(AgentUserMessage),
    /// 普通状态、模型、警告轨迹。
    Trace(AgentTraceEntry),
    /// 连续工具调用组；默认只展示最后一条。
    ToolGroup {
        traces: Vec<AgentTraceEntry>,
        is_expanded: bool,
    },
    /// 模型思考过程，仅在当前内存会话展示。
    Reasoning(String),
    /// 模型最终可见回答及其可信引用。
    Answer {
        content: String,
        citations: Vec<AssistantCitation>,
    },
}

/// 主窗口内嵌 Agent 助手实体。
pub(crate) struct AssistantPanel {
    /// 主窗口应用实体，用于来源扫描回填、设置入口和证据导航。
    app: Entity<ArgusApp>,
    /// 当前主题快照。
    theme: AppTheme,
    /// 已启用模型快照；由主应用在创建面板或保存设置时主动注入。
    models: Vec<AiModelProfile>,
    /// 当前选择模型的稳定配置 ID。
    selected_model_id: Option<String>,
    /// 当前模型最近一次系统凭据检查结果。
    credential_error: Option<String>,
    /// 主应用注入的来源可用状态；渲染期间不得回读正在渲染的父实体。
    has_loaded_sources: bool,
    /// 当前会话全部来源快照。
    scope: Option<Arc<SourceScopeSnapshot>>,
    /// 当前快照可选择的文件和目录候选。
    mention_candidates: Arc<Vec<AssistantMentionCandidate>>,
    /// 当前“@”查询；为空时不展示候选面板。
    mention_query: Option<AssistantMentionQuery>,
    /// 当前查询命中的候选下标，最多展示固定数量。
    mention_results: Vec<usize>,
    /// 键盘当前高亮的候选行。
    mention_highlighted_index: usize,
    /// 当前编辑消息已选择的可信来源。
    selected_mentions: Vec<AssistantSelectedMention>,
    /// 可见消息 ID 到模型实际消息的映射，用于隐藏 Argus 生成的可信来源元数据。
    pending_runtime_messages: HashMap<String, String>,
    /// 创建快照后应用层来源内容版本。
    scope_revision: Option<u64>,
    /// 已完成的跨 Provider 中立历史。
    history: Vec<AssistantHistoryTurn>,
    /// 当前消息流；使用 Arc 让虚拟列表渲染闭包只做轻量克隆。
    messages: Arc<Vec<AssistantPanelMessage>>,
    /// 可变高度虚拟列表状态。
    message_list: ListState,
    /// 用户主动上滚后暂停自动跟随最新消息。
    is_following_latest: bool,
    /// 是否已经注册列表滚动监听。
    has_registered_scroll_handler: bool,
    /// 滚动条拖动时鼠标相对滑块顶部的偏移。
    scrollbar_drag_offset: Option<Pixels>,
    /// 底部多行输入状态。
    input: TextInputState,
    /// 多行输入滚动句柄。
    input_scroll: ScrollHandle,
    /// 多行输入自绘滚动状态。
    input_scroll_state: TextareaScrollState,
    /// 输入焦点句柄。
    input_focus: FocusHandle,
    /// 当前状态。
    status: AssistantPanelStatus,
    /// 最近一次可展示错误。
    error: Option<String>,
    /// 当前回答 Token 用量。
    budget: AgentBudgetSnapshot,
    /// 当前回答追加消息发送端。
    user_message_sender: Option<async_channel::Sender<AgentUserMessage>>,
    /// 当前回答未消费消息计数。
    pending_user_messages: Arc<AtomicUsize>,
    /// 当前回答取消令牌。
    turn_cancellation: Option<tokio_util::sync::CancellationToken>,
    /// 当前来源扫描取消令牌。
    scan_cancellation: Option<tokio_util::sync::CancellationToken>,
    /// 来源扫描 generation，拒绝取消或重置后的迟到结果。
    scan_generation: u64,
    /// 回答 generation，拒绝上一轮事件污染新回答。
    turn_generation: u64,
    /// 当前回答开始追加流式消息的位置，用于自动重试时精确移除未完成正文。
    active_turn_message_start: Option<usize>,
    /// 当前回答已经实际交给模型的用户消息 ID；配置失效时据此原子恢复排队状态。
    active_turn_user_message_ids: Vec<String>,
    /// 主题观察订阅。
    _theme_observer: Subscription,
}

impl AssistantPanel {
    /// 创建空闲、内存态且尚未扫描来源的助手面板。
    ///
    /// `ArgusApp` 会在自身更新事务中延迟创建本实体，因此构造过程不能通过
    /// `app.read(cx)` 回读父实体，否则 GPUI 会因同一实体重入借用而直接 panic。
    pub(crate) fn new(
        app: Entity<ArgusApp>,
        theme: AppTheme,
        ai_config: AiConfig,
        has_loaded_sources: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let _theme_observer = observe_app_theme(cx, &app, theme.clone(), |panel, theme, _| {
            panel.theme = theme.clone();
        });
        let welcome = AssistantPanelMessage::Trace(AgentTraceEntry::new(
            AgentTraceKind::Status,
            "Agent 助手",
            "输入问题后将完整扫描全部已加载来源，并按需调用只读日志工具。",
        ));
        let mut panel = Self {
            app,
            theme,
            models: Vec::new(),
            selected_model_id: None,
            credential_error: None,
            has_loaded_sources,
            scope: None,
            mention_candidates: Arc::new(Vec::new()),
            mention_query: None,
            mention_results: Vec::new(),
            mention_highlighted_index: 0,
            selected_mentions: Vec::new(),
            pending_runtime_messages: HashMap::new(),
            scope_revision: None,
            history: Vec::new(),
            messages: Arc::new(vec![welcome]),
            message_list: ListState::new(1, ListAlignment::Bottom, px(ASSISTANT_LIST_OVERDRAW)),
            is_following_latest: true,
            has_registered_scroll_handler: false,
            scrollbar_drag_offset: None,
            input: TextInputState::default(),
            input_scroll: ScrollHandle::new(),
            input_scroll_state: TextareaScrollState::new(),
            input_focus: cx.focus_handle(),
            status: AssistantPanelStatus::Idle,
            error: None,
            budget: AgentBudgetSnapshot::default(),
            user_message_sender: None,
            pending_user_messages: Arc::new(AtomicUsize::new(0)),
            turn_cancellation: None,
            scan_cancellation: None,
            scan_generation: 0,
            turn_generation: 0,
            active_turn_message_start: None,
            active_turn_user_message_ids: Vec::new(),
            _theme_observer,
        };
        panel.apply_model_configuration(ai_config);
        if panel.has_loaded_sources {
            panel.schedule_source_scan(cx);
        }
        panel
    }

    /// 接受来源注册表的非替换式补齐，保留当前上下文和可信不可变快照。
    ///
    /// 若补齐发生在扫描期间，则取消旧扫描并自动重试，避免旧结果覆盖较新的来源树。
    pub(crate) fn accept_source_registry_revision(
        &mut self,
        revision: u64,
        has_loaded_sources: bool,
        cx: &mut Context<Self>,
    ) {
        self.has_loaded_sources = has_loaded_sources;
        self.scope_revision = Some(revision);
        if self.status == AssistantPanelStatus::Scanning {
            if let Some(cancellation) = self.scan_cancellation.take() {
                cancellation.cancel();
            }
            self.scan_generation = self.scan_generation.wrapping_add(1);
            self.scope = None;
            self.clear_source_mention_state();
            self.status = AssistantPanelStatus::Idle;
            self.schedule_source_scan(cx);
        } else if self.scope.is_none()
            && self.status == AssistantPanelStatus::Idle
            && has_loaded_sources
        {
            self.schedule_source_scan(cx);
        }
    }

    /// 日志说明或原文授权变化时仅废弃工具范围，保留用户可见会话并自动重建范围。
    pub(crate) fn invalidate_scope_for_analysis_configuration_change(
        &mut self,
        revision: u64,
        reason: &'static str,
        cx: &mut Context<Self>,
    ) {
        let requeued_active_turn = self.cancel_background_work_and_requeue_active_turn();
        self.scope = None;
        self.clear_source_mention_state();
        self.scope_revision = Some(revision);
        self.status = AssistantPanelStatus::Idle;
        self.error = None;
        self.push_trace(AgentTraceEntry::new(
            AgentTraceKind::Status,
            "智能分析配置已更新",
            format!(
                "{reason}，正在重新扫描日志范围；现有对话上下文继续保留{}。",
                if requeued_active_turn {
                    "，当前问题已重新排队"
                } else {
                    ""
                }
            ),
        ));
        if self.has_loaded_sources {
            self.schedule_source_scan(cx);
        }
    }

    /// 延迟到当前 GPUI 更新事务结束后启动来源扫描，避免子实体回读正在更新的父实体。
    fn schedule_source_scan(&mut self, cx: &mut Context<Self>) {
        if !self.has_loaded_sources || self.status.is_busy() {
            return;
        }
        let expected_generation = self.scan_generation;
        cx.spawn(async move |view, cx| {
            Timer::after(Duration::from_millis(1)).await;
            view.update(cx, |panel, panel_cx| {
                if panel.scan_generation != expected_generation
                    || panel.status != AssistantPanelStatus::Idle
                    || panel.scope.is_some()
                    || !panel.has_loaded_sources
                {
                    return;
                }
                panel.start_source_scan(panel_cx);
                panel_cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 应用主窗口注入的模型配置，保留仍有效的选择并重新检查对应系统凭据。
    ///
    /// 配置通过值传递，避免内嵌面板构造或渲染期间读取父 `ArgusApp` 造成实体重入。
    pub(crate) fn apply_model_configuration(&mut self, mut config: AiConfig) {
        config.normalize();
        let models = config
            .model_profiles
            .into_iter()
            .filter(|model| model.enabled)
            .collect::<Vec<_>>();
        let selection_is_valid = self
            .selected_model_id
            .as_deref()
            .is_some_and(|selected| models.iter().any(|model| model.profile_id == selected));
        if !selection_is_valid {
            self.selected_model_id = models.first().map(|model| model.profile_id.clone());
        }
        self.models = models;
        // 即使模型字段没有变化也要重读凭据，因为用户可能只更新了系统密钥库中的 API Key。
        self.refresh_selected_model_credential();
    }

    /// 检查当前模型凭据并缓存用户可读错误，避免每个流式 Token 批次同步访问系统密钥库。
    fn refresh_selected_model_credential(&mut self) {
        self.credential_error = self
            .selected_model()
            .and_then(|model| load_api_key(&model.base_url).err());
    }

    /// 返回当前选择模型。
    fn selected_model(&self) -> Option<&AiModelProfile> {
        let selected = self.selected_model_id.as_deref()?;
        self.models
            .iter()
            .find(|model| model.profile_id == selected)
    }

    /// 返回当前不可发送的产品原因；运行态仍允许发送补充消息。
    ///
    /// 未加载日志的空态在渲染入口已直接拦截，这里只需覆盖模型和凭据问题。
    fn unavailable_reason(&self) -> Option<String> {
        if self.selected_model().is_none() {
            return Some("尚未配置已启用模型，请先完成模型配置。".to_string());
        }
        self.credential_error.clone()
    }

    /// 循环选择下一个已启用模型；运行中禁止切换，避免一轮内混用 Provider 协议。
    fn select_next_model(&mut self) {
        if self.status.is_busy() || self.models.len() <= 1 {
            return;
        }
        let current = self
            .selected_model_id
            .as_deref()
            .and_then(|id| self.models.iter().position(|model| model.profile_id == id))
            .unwrap_or(0);
        let next = (current + 1) % self.models.len();
        self.selected_model_id = Some(self.models[next].profile_id.clone());
        self.refresh_selected_model_credential();
        self.error = None;
    }

    /// 处理发送按钮或 Cmd/Ctrl+Enter。
    fn submit_input(&mut self, cx: &mut Context<Self>) {
        let content = self.input.value.trim().to_string();
        if content.is_empty() {
            self.error = Some("请输入需要分析的问题".to_string());
            return;
        }
        if content.len() > ASSISTANT_MESSAGE_MAX_BYTES {
            self.error = Some("单条消息不能超过 32 KiB".to_string());
            return;
        }
        if self.models.is_empty() {
            self.error = Some("尚未配置已启用模型".to_string());
            return;
        }
        let current_revision = self.app.read(cx).source_content_revision;
        if self
            .scope_revision
            .is_some_and(|revision| revision != current_revision)
        {
            self.accept_source_registry_revision(current_revision, true, cx);
        }

        let runtime_content = assistant_message_with_mentions(&content, &self.selected_mentions);
        let message = AgentUserMessage::queued(content);
        self.pending_runtime_messages
            .insert(message.message_id.clone(), runtime_content);
        self.push_message(AssistantPanelMessage::User(message.clone()));
        self.input = TextInputState::default();
        self.input.is_focused = true;
        self.mention_query = None;
        self.mention_results.clear();
        self.selected_mentions.clear();
        self.error = None;
        match self.status {
            AssistantPanelStatus::Scanning => {}
            AssistantPanelStatus::Running => self.queue_running_message(message),
            AssistantPanelStatus::Cancelling => {
                // 消息保留为排队状态，当前回答停止后由下一次发送或自动续答消费。
            }
            AssistantPanelStatus::Idle => {
                if self.scope.is_none() {
                    self.start_source_scan(cx);
                } else {
                    self.start_queued_turn(cx);
                }
            }
        }
    }

    /// 把运行中补充写入当前模型边界队列；通道关闭时保持排队供下一轮消费。
    fn queue_running_message(&mut self, message: AgentUserMessage) {
        let Some(sender) = self.user_message_sender.as_ref() else {
            return;
        };
        let mut runtime_message = message.clone();
        if let Some(runtime_content) = self.pending_runtime_messages.get(&message.message_id) {
            runtime_message.content = runtime_content.clone();
        }
        self.pending_user_messages.fetch_add(1, Ordering::AcqRel);
        if sender.try_send(runtime_message).is_err() {
            self.pending_user_messages.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// 启动全来源树扫描；扫描结果回填主应用并成为当前会话可信范围。
    fn start_source_scan(&mut self, cx: &mut Context<Self>) {
        if self.status != AssistantPanelStatus::Idle {
            return;
        }
        let mut config = self.app.read(cx).config.ai.clone();
        config.normalize();
        let (registry, default_encoding, loader_config, base_revision) = {
            let app = self.app.read(cx);
            if app.source_registry.root_ids().is_empty() {
                self.error = Some("尚未加载日志来源".to_string());
                return;
            }
            (
                app.source_registry.clone(),
                app.selected_encoding.clone(),
                app.config.loader.clone(),
                app.source_content_revision,
            )
        };

        self.scan_generation = self.scan_generation.wrapping_add(1);
        let generation = self.scan_generation;
        let cancellation = tokio_util::sync::CancellationToken::new();
        self.scan_cancellation = Some(cancellation.clone());
        self.status = AssistantPanelStatus::Scanning;
        self.push_trace(AgentTraceEntry::new(
            AgentTraceKind::Status,
            "正在扫描全部来源",
            "正在补齐未展开目录和归档，并匹配日志类型说明",
        ));
        cx.spawn(async move |view, cx| {
            let preparation = cx
                .background_executor()
                .spawn(async move {
                    prepare_agent_source_scope_for_selection(
                        registry,
                        AgentScopeSelection::AllLoadedRoots,
                        config,
                        default_encoding,
                        loader_config,
                        cancellation,
                    )
                })
                .await;
            view.update(cx, |panel, panel_cx| {
                panel.finish_source_scan(generation, base_revision, preparation, panel_cx);
                panel_cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 接收扫描结果并原子回填来源注册表；外部来源变化会让本结果失效。
    fn finish_source_scan(
        &mut self,
        generation: u64,
        base_revision: u64,
        preparation: Result<crate::agent::AgentSourcePreparation, String>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.scan_generation {
            return;
        }
        self.scan_cancellation = None;
        let preparation = match preparation {
            Ok(preparation) => preparation,
            Err(error) => {
                self.status = AssistantPanelStatus::Idle;
                if !error.contains("已取消") {
                    self.error = Some(format!("来源树完整扫描失败：{error}"));
                    self.push_trace(AgentTraceEntry::new(
                        AgentTraceKind::Warning,
                        "来源扫描失败",
                        error,
                    ));
                }
                return;
            }
        };
        let source_count = preparation.scope.sources.len();
        let profile_count = preparation.scope.profiles.len();
        let warning_count = preparation.warnings.len();
        let registry = preparation.registry;
        let scope = Arc::new(preparation.scope);
        let mention_candidates = Arc::new(build_mention_candidates(&scope));
        let revision = self.app.update(cx, |app, _| {
            app.apply_assistant_scanned_registry(base_revision, registry)
        });
        let revision = match revision {
            Ok(revision) => revision,
            Err(error) => {
                let current_revision = self.app.read(cx).source_content_revision;
                self.status = AssistantPanelStatus::Idle;
                self.accept_source_registry_revision(current_revision, true, cx);
                self.push_trace(AgentTraceEntry::new(
                    AgentTraceKind::Status,
                    "来源扫描已重新调度",
                    error,
                ));
                return;
            }
        };
        self.scope = Some(scope);
        self.mention_candidates = mention_candidates;
        self.refresh_mention_picker();
        self.scope_revision = Some(revision);
        self.has_loaded_sources = source_count > 0;
        self.status = AssistantPanelStatus::Idle;
        self.push_trace(AgentTraceEntry::new(
            AgentTraceKind::Status,
            "全部来源扫描完成",
            format!(
                "已固化 {source_count} 个日志文件，匹配 {profile_count} 种日志类型说明{}",
                if warning_count == 0 {
                    String::new()
                } else {
                    format!("，包含 {warning_count} 项可容忍警告")
                }
            ),
        ));
        self.start_queued_turn(cx);
    }

    /// 把当前所有排队消息作为一轮初始用户输入，并启动独立可取消的模型工具循环。
    fn start_queued_turn(&mut self, cx: &mut Context<Self>) {
        if self.status != AssistantPanelStatus::Idle {
            return;
        }
        let queued_messages = self
            .messages
            .iter()
            .filter_map(|message| match message {
                AssistantPanelMessage::User(message)
                    if message.status == AgentUserMessageStatus::Queued =>
                {
                    Some((
                        message.message_id.clone(),
                        self.pending_runtime_messages
                            .get(&message.message_id)
                            .cloned()
                            .unwrap_or_else(|| message.content.clone()),
                    ))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        if queued_messages.is_empty() {
            return;
        }
        let Some(scope) = self.scope.clone() else {
            self.error = Some("来源范围尚未准备完成".to_string());
            return;
        };
        let mut config = self.app.read(cx).config.ai.clone();
        config.normalize();
        let Some(model) = self.selected_model().cloned() else {
            self.error = Some("当前模型配置已经失效".to_string());
            return;
        };
        let api_key = match load_api_key(&model.base_url) {
            Ok(api_key) => api_key,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        // 所有可能失败的前置检查完成后才提交界面状态，避免密钥或配置异常吞掉排队消息。
        let active_message_ids = queued_messages
            .iter()
            .map(|(message_id, _)| message_id.clone())
            .collect::<Vec<_>>();
        let initial_messages = queued_messages
            .into_iter()
            .map(|(_, content)| content)
            .collect::<Vec<_>>();
        let messages = Arc::make_mut(&mut self.messages);
        let mut changed_from = messages.len();
        for (index, message) in messages.iter_mut().enumerate() {
            if let AssistantPanelMessage::User(message) = message
                && active_message_ids.contains(&message.message_id)
            {
                message.status = AgentUserMessageStatus::Consumed;
                changed_from = changed_from.min(index);
            }
        }
        if changed_from < messages.len() {
            self.message_list
                .splice(changed_from..messages.len(), messages.len() - changed_from);
        }
        let cancellation = tokio_util::sync::CancellationToken::new();
        let (user_message_sender, user_message_receiver) = async_channel::bounded(20);
        let pending_user_messages = Arc::new(AtomicUsize::new(0));
        let (event_sender, event_receiver) = async_channel::bounded(256);
        self.turn_generation = self.turn_generation.wrapping_add(1);
        let generation = self.turn_generation;
        self.turn_cancellation = Some(cancellation.clone());
        self.user_message_sender = Some(user_message_sender);
        self.pending_user_messages = pending_user_messages.clone();
        self.status = AssistantPanelStatus::Running;
        self.active_turn_message_start = Some(self.messages.len());
        self.active_turn_user_message_ids = active_message_ids;
        self.error = None;
        self.budget = AgentBudgetSnapshot::default();
        self.poll_turn_events(generation, event_receiver, cx);
        agent_runtime().spawn(run_assistant_turn(AssistantRunRequest {
            initial_user_messages: initial_messages,
            history: self.history.clone(),
            config,
            model,
            scope,
            api_key,
            cancellation,
            user_message_receiver,
            event_sender,
            pending_user_messages,
        }));
    }

    /// 批量接收一轮后台事件，防止每个 Token 单独触发主窗口重绘。
    fn poll_turn_events(
        &self,
        generation: u64,
        event_receiver: async_channel::Receiver<AgentEvent>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |view, cx| {
            while let Ok(first_event) = event_receiver.recv().await {
                let mut events = vec![first_event];
                Timer::after(ASSISTANT_EVENT_BATCH_INTERVAL).await;
                while events.len() < 128 {
                    let Ok(event) = event_receiver.try_recv() else {
                        break;
                    };
                    events.push(event);
                }
                if view
                    .update(cx, |panel, panel_cx| {
                        if panel.turn_generation != generation {
                            return;
                        }
                        for event in events {
                            panel.apply_turn_event(event, generation, panel_cx);
                        }
                        panel_cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// 应用助手后台事件，并把同类流增量合并为单条虚拟消息。
    fn apply_turn_event(&mut self, event: AgentEvent, generation: u64, cx: &mut Context<Self>) {
        if generation != self.turn_generation {
            return;
        }
        match event {
            AgentEvent::Status(status) => match status {
                AgentSessionStatus::Investigating => self.status = AssistantPanelStatus::Running,
                AgentSessionStatus::Cancelled => {
                    self.finish_active_turn();
                    self.push_trace(AgentTraceEntry::new(
                        AgentTraceKind::Status,
                        "当前回答已停止",
                        "对话和已验证引用仍保留，可继续发送新问题",
                    ));
                    // 用户停止只终止当前回答；停止前尚未进入模型边界的补充应自动成为下一轮。
                    if self.has_queued_messages() {
                        self.start_queued_turn(cx);
                    }
                }
                AgentSessionStatus::Failed => self.finish_active_turn(),
                _ => {}
            },
            AgentEvent::Trace(trace) => self.push_trace(trace),
            AgentEvent::Budget(budget) => self.budget = budget,
            AgentEvent::StreamDelta(kind, delta) => self.apply_stream_delta(kind, delta),
            AgentEvent::UserMessageConsumed(message_id) => {
                self.update_user_message_status(&message_id, AgentUserMessageStatus::Consumed);
                if !self
                    .active_turn_user_message_ids
                    .iter()
                    .any(|active_id| active_id == &message_id)
                {
                    self.active_turn_user_message_ids.push(message_id);
                }
            }
            AgentEvent::UserMessageRejected(message_id, reason) => {
                self.update_user_message_status(&message_id, AgentUserMessageStatus::Rejected);
                // 已明确拒绝的补充不会进入后续轮次，隐藏来源绑定也应随消息终态释放。
                self.pending_runtime_messages.remove(&message_id);
                self.push_trace(AgentTraceEntry::new(
                    AgentTraceKind::Warning,
                    "补充消息未进入当前回答",
                    reason,
                ));
            }
            AgentEvent::AssistantCompleted {
                output,
                citations,
                accepted_user_messages,
                history_was_trimmed,
            } => {
                self.finish_answer(output.clone(), citations.clone());
                self.history.push(AssistantHistoryTurn {
                    user_messages: accepted_user_messages,
                    assistant_output: output,
                    citations,
                });
                self.finish_active_turn();
                if history_was_trimmed {
                    self.push_trace(AgentTraceEntry::new(
                        AgentTraceKind::Status,
                        "上下文窗口已整理",
                        "较早对话仍保留在界面中，但已从本轮模型上下文移除",
                    ));
                }
                // 回答结束前到达但尚未被 Hook 消费的消息自动成为下一轮。
                if self.has_queued_messages() {
                    self.start_queued_turn(cx);
                }
            }
            AgentEvent::AssistantAttemptReset => self.reset_incomplete_stream_messages(),
            AgentEvent::Failed(message) => {
                self.error = Some(message.clone());
                self.push_trace(AgentTraceEntry::new(
                    AgentTraceKind::Warning,
                    "助手回答失败",
                    message,
                ));
            }
            // 固定分析专属事件不会由交互助手发布。
            AgentEvent::Stage(_) | AgentEvent::Report(_, _) => {}
        }
    }

    /// 合并相邻同类流式内容。
    fn apply_stream_delta(&mut self, kind: AgentStreamKind, delta: String) {
        if delta.is_empty() {
            return;
        }
        let messages = Arc::make_mut(&mut self.messages);
        let last_index = messages.len().saturating_sub(1);
        match (kind, messages.last_mut()) {
            (AgentStreamKind::Reasoning, Some(AssistantPanelMessage::Reasoning(content))) => {
                content.push_str(&delta);
                self.message_list.splice(last_index..last_index + 1, 1);
            }
            (AgentStreamKind::Output, Some(AssistantPanelMessage::Answer { content, .. })) => {
                content.push_str(&delta);
                self.message_list.splice(last_index..last_index + 1, 1);
            }
            (AgentStreamKind::Reasoning, _) => {
                self.push_message(AssistantPanelMessage::Reasoning(delta));
            }
            (AgentStreamKind::Output, _) => {
                self.push_message(AssistantPanelMessage::Answer {
                    content: delta,
                    citations: Vec::new(),
                });
            }
        }
    }

    /// 使用最终响应校正最后一条流式正文并挂载可信引用。
    fn finish_answer(&mut self, output: String, citations: Vec<AssistantCitation>) {
        let messages = Arc::make_mut(&mut self.messages);
        let turn_start = self
            .active_turn_message_start
            .unwrap_or(messages.len())
            .min(messages.len());
        let answer_index = (turn_start..messages.len())
            .rev()
            .find(|index| matches!(messages[*index], AssistantPanelMessage::Answer { .. }));
        if let Some(index) = answer_index
            && let AssistantPanelMessage::Answer {
                content,
                citations: refs,
            } = &mut messages[index]
        {
            if !output.trim().is_empty() {
                *content = output;
            }
            *refs = citations;
            self.message_list.splice(index..index + 1, 1);
        } else {
            self.push_message(AssistantPanelMessage::Answer {
                content: output,
                citations,
            });
        }
    }

    /// 工具轨迹连续到达时合并为一组，其余轨迹保持独立消息。
    fn push_trace(&mut self, trace: AgentTraceEntry) {
        if trace.kind == AgentTraceKind::Tool {
            let messages = Arc::make_mut(&mut self.messages);
            if let Some(AssistantPanelMessage::ToolGroup { traces, .. }) = messages.last_mut() {
                traces.push(trace);
                let index = messages.len() - 1;
                self.message_list.splice(index..index + 1, 1);
                return;
            }
            self.push_message(AssistantPanelMessage::ToolGroup {
                traces: vec![trace],
                is_expanded: false,
            });
        } else {
            self.push_message(AssistantPanelMessage::Trace(trace));
        }
    }

    /// 追加消息并只让虚拟列表测量新增行。
    fn push_message(&mut self, message: AssistantPanelMessage) {
        let messages = Arc::make_mut(&mut self.messages);
        let old_len = messages.len();
        messages.push(message);
        self.message_list.splice(old_len..old_len, 1);
    }

    /// 替换整段会话消息，用于测试直接构造消息边界场景。
    #[cfg(test)]
    fn replace_messages(&mut self, messages: Vec<AssistantPanelMessage>) {
        self.messages = Arc::new(messages);
        self.message_list.reset(self.messages.len());
        self.is_following_latest = true;
    }

    /// 更新指定用户消息状态并失效对应虚拟行。
    fn update_user_message_status(&mut self, message_id: &str, status: AgentUserMessageStatus) {
        let messages = Arc::make_mut(&mut self.messages);
        if let Some((index, AssistantPanelMessage::User(message))) = messages
            .iter_mut()
            .enumerate()
            .find(|(_, item)| {
                matches!(item, AssistantPanelMessage::User(message) if message.message_id == message_id)
            })
        {
            message.status = status;
            self.message_list.splice(index..index + 1, 1);
        }
    }

    /// 返回是否还存在未被当前回答消费的用户消息。
    fn has_queued_messages(&self) -> bool {
        self.messages.iter().any(|message| {
            matches!(message, AssistantPanelMessage::User(message) if message.status == AgentUserMessageStatus::Queued)
        })
    }

    /// 停止当前来源扫描或模型回答；对话历史和面板实体不销毁。
    fn stop_current_work(&mut self) {
        if !self.status.is_busy() || self.status == AssistantPanelStatus::Cancelling {
            return;
        }
        self.status = AssistantPanelStatus::Cancelling;
        if let Some(cancellation) = self.scan_cancellation.as_ref() {
            cancellation.cancel();
        }
        if let Some(cancellation) = self.turn_cancellation.as_ref() {
            cancellation.cancel();
        }
    }

    /// 清理当前回答通道，让后续发送创建新的模型工具循环。
    fn finish_active_turn(&mut self) {
        for message_id in self.active_turn_user_message_ids.drain(..) {
            self.pending_runtime_messages.remove(&message_id);
        }
        self.status = AssistantPanelStatus::Idle;
        self.user_message_sender = None;
        self.turn_cancellation = None;
        self.pending_user_messages = Arc::new(AtomicUsize::new(0));
        self.active_turn_message_start = None;
    }

    /// 取消旧配置下的后台任务，并把尚未形成完整回答的当前用户消息恢复为排队状态。
    ///
    /// 返回值：存在运行中的模型轮次且至少恢复了一条用户消息时返回 `true`。
    fn cancel_background_work_and_requeue_active_turn(&mut self) -> bool {
        if let Some(cancellation) = self.scan_cancellation.take() {
            cancellation.cancel();
        }
        if let Some(cancellation) = self.turn_cancellation.take() {
            cancellation.cancel();
        }
        self.scan_generation = self.scan_generation.wrapping_add(1);
        self.turn_generation = self.turn_generation.wrapping_add(1);

        let should_requeue = self.status == AssistantPanelStatus::Running;
        let active_message_ids = std::mem::take(&mut self.active_turn_user_message_ids);
        if should_requeue {
            let messages = Arc::make_mut(&mut self.messages);
            for message in messages.iter_mut() {
                if let AssistantPanelMessage::User(message) = message
                    && active_message_ids.contains(&message.message_id)
                {
                    message.status = AgentUserMessageStatus::Queued;
                }
            }
            if let Some(start) = self.active_turn_message_start
                && start < messages.len()
            {
                // 配置变化后的重跑不能保留旧配置生成的思考、工具轨迹或半截回答。
                let retained_users = messages
                    .drain(start..)
                    .filter(|message| matches!(message, AssistantPanelMessage::User(_)))
                    .collect::<Vec<_>>();
                messages.extend(retained_users);
            }
            self.message_list.reset(messages.len());
        } else {
            for message_id in &active_message_ids {
                self.pending_runtime_messages.remove(message_id);
            }
        }

        self.active_turn_message_start = None;
        self.user_message_sender = None;
        self.pending_user_messages = Arc::new(AtomicUsize::new(0));
        should_requeue && !active_message_ids.is_empty()
    }

    /// 自动重试前移除本轮未完成的思考和回答，保留用户补充、工具轨迹及失败说明。
    fn reset_incomplete_stream_messages(&mut self) {
        let Some(start) = self.active_turn_message_start else {
            return;
        };
        let messages = Arc::make_mut(&mut self.messages);
        if start >= messages.len() {
            return;
        }
        let retained = messages
            .drain(start..)
            .filter(|message| {
                !matches!(
                    message,
                    AssistantPanelMessage::Reasoning(_) | AssistantPanelMessage::Answer { .. }
                )
            })
            .collect::<Vec<_>>();
        messages.extend(retained);
        self.message_list.reset(messages.len());
        if self.is_following_latest {
            self.jump_to_latest();
        }
    }

    /// 取消全部后台 generation，供分析配置更新和实体销毁复用。
    fn cancel_background_work(&mut self) {
        if let Some(cancellation) = self.scan_cancellation.take() {
            cancellation.cancel();
        }
        if let Some(cancellation) = self.turn_cancellation.take() {
            cancellation.cancel();
        }
        self.scan_generation = self.scan_generation.wrapping_add(1);
        self.turn_generation = self.turn_generation.wrapping_add(1);
        for message_id in self.active_turn_user_message_ids.drain(..) {
            self.pending_runtime_messages.remove(&message_id);
        }
        self.active_turn_message_start = None;
        self.user_message_sender = None;
        self.pending_user_messages = Arc::new(AtomicUsize::new(0));
    }

    /// 清除依赖旧来源快照的候选、选择和尚未提交的运行时绑定。
    fn clear_source_mention_state(&mut self) {
        self.mention_candidates = Arc::new(Vec::new());
        self.mention_query = None;
        self.mention_results.clear();
        self.mention_highlighted_index = 0;
        self.selected_mentions.clear();
        self.pending_runtime_messages.clear();
    }

    /// 根据光标处的“@”查询筛选文件和文件夹候选。
    fn refresh_mention_picker(&mut self) {
        self.mention_query = active_mention_query(&self.input);
        self.mention_results.clear();
        self.mention_highlighted_index = 0;
        let Some(query) = self.mention_query.as_ref() else {
            return;
        };
        let normalized_query = query.text.to_lowercase();
        let mut matches = self
            .mention_candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| {
                normalized_query.is_empty() || candidate.search_key.contains(&normalized_query)
            })
            .map(|(index, candidate)| {
                let name = candidate
                    .display_path
                    .rsplit('/')
                    .next()
                    .unwrap_or(candidate.display_path.as_str())
                    .to_lowercase();
                let rank = if normalized_query.is_empty() {
                    1
                } else if name.starts_with(&normalized_query) {
                    0
                } else {
                    1
                };
                (rank, candidate.kind == AssistantMentionKind::File, index)
            })
            .collect::<Vec<_>>();
        matches.sort_by_key(|(rank, is_file, index)| (*rank, *is_file, *index));
        self.mention_results.extend(
            matches
                .into_iter()
                .take(ASSISTANT_MENTION_RESULT_LIMIT)
                .map(|(_, _, index)| index),
        );
    }

    /// 把当前高亮候选插入输入框，并保存其受信来源绑定。
    fn select_highlighted_mention(&mut self) -> bool {
        let Some(query) = self.mention_query.clone() else {
            return false;
        };
        let Some(candidate_index) = self
            .mention_results
            .get(self.mention_highlighted_index)
            .copied()
        else {
            return false;
        };
        self.select_mention(query, candidate_index)
    }

    /// 选择指定候选；替换范围按 Unicode 字符计算，不允许候选路径被解释为磁盘路径。
    fn select_mention(&mut self, query: AssistantMentionQuery, candidate_index: usize) -> bool {
        let Some(candidate) = self.mention_candidates.get(candidate_index).cloned() else {
            return false;
        };
        let marker = format!("@{}", candidate.display_path);
        let replacement = format!("{marker} ");
        self.input.value =
            replace_character_range(&self.input.value, query.range.clone(), &replacement);
        self.input.cursor = query.range.start + character_count(&replacement);
        self.input.selection_anchor = None;
        self.input.marked_range = None;
        if !self.selected_mentions.iter().any(|selected| {
            selected.candidate.kind == candidate.kind
                && selected.candidate.display_path == candidate.display_path
        }) {
            self.selected_mentions
                .push(AssistantSelectedMention { marker, candidate });
        }
        self.mention_query = None;
        self.mention_results.clear();
        self.mention_highlighted_index = 0;
        self.error = None;
        true
    }

    /// 处理文本域键盘输入。
    fn handle_input_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.mention_query.is_some() {
            match event.keystroke.key.to_ascii_lowercase().as_str() {
                "up" | "arrowup" if !self.mention_results.is_empty() => {
                    self.mention_highlighted_index = self
                        .mention_highlighted_index
                        .checked_sub(1)
                        .unwrap_or(self.mention_results.len() - 1);
                    return;
                }
                "down" | "arrowdown" if !self.mention_results.is_empty() => {
                    self.mention_highlighted_index =
                        (self.mention_highlighted_index + 1) % self.mention_results.len();
                    return;
                }
                "enter" | "tab" if !event.keystroke.modifiers.secondary() => {
                    if self.select_highlighted_mention() {
                        return;
                    }
                }
                "escape" => {
                    self.mention_query = None;
                    self.mention_results.clear();
                    return;
                }
                _ => {}
            }
        }
        match handle_local_input_key(&mut self.input, &event.keystroke, true, cx) {
            LocalInputAction::Submit => self.submit_input(cx),
            LocalInputAction::Changed => {
                self.error = None;
                self.refresh_mention_picker();
            }
            LocalInputAction::Close => {
                self.mention_query = None;
                self.mention_results.clear();
                self.input.clear_focus();
            }
            LocalInputAction::None => {}
        }
    }

    /// 展开或收起指定工具轨迹组。
    fn toggle_tool_group(&mut self, index: usize) {
        let messages = Arc::make_mut(&mut self.messages);
        if let Some(AssistantPanelMessage::ToolGroup { is_expanded, .. }) = messages.get_mut(index)
        {
            *is_expanded = !*is_expanded;
            self.message_list.splice(index..index + 1, 1);
        }
    }

    /// 跳转消息列表底部并恢复流式自动跟随。
    fn jump_to_latest(&mut self) {
        self.is_following_latest = true;
        let max_offset = self.message_list.max_offset_for_scrollbar().height;
        self.message_list
            .set_offset_from_scrollbar(point(px(0.0), -max_offset));
    }
}

/// 从来源快照生成目录和文件候选；目录只保存后代计数，避免为大目录复制数千个引用。
fn build_mention_candidates(scope: &SourceScopeSnapshot) -> Vec<AssistantMentionCandidate> {
    let mut folder_counts = BTreeMap::<String, usize>::new();
    let mut files = Vec::with_capacity(scope.sources.len());
    for source in scope.sources.iter() {
        let components = source.relative_path.split('/').collect::<Vec<_>>();
        for component_count in 1..components.len() {
            let folder_path = components[..component_count].join("/");
            *folder_counts.entry(folder_path).or_default() += 1;
        }
        files.push(AssistantMentionCandidate {
            display_path: source.relative_path.clone(),
            search_key: source.relative_path.to_lowercase(),
            kind: AssistantMentionKind::File,
            source_ref: Some(source.source_ref.clone()),
            matching_source_count: 1,
        });
    }
    files.sort_by(|left, right| left.display_path.cmp(&right.display_path));
    let mut candidates = folder_counts
        .into_iter()
        .map(
            |(display_path, matching_source_count)| AssistantMentionCandidate {
                search_key: display_path.to_lowercase(),
                display_path,
                kind: AssistantMentionKind::Folder,
                source_ref: None,
                matching_source_count,
            },
        )
        .collect::<Vec<_>>();
    candidates.extend(files);
    candidates
}

/// 解析光标所在的不含空白 token；仅以 token 起始的 `@` 触发，避免普通邮箱文本误弹候选。
fn active_mention_query(input: &TextInputState) -> Option<AssistantMentionQuery> {
    if input.marked_range.is_some() || input.selection_range().is_some() {
        return None;
    }
    let characters = input.value.chars().collect::<Vec<_>>();
    let cursor = input.cursor.min(characters.len());
    let token_start = characters[..cursor]
        .iter()
        .rposition(|character| character.is_whitespace())
        .map_or(0, |index| index + 1);
    if characters.get(token_start) != Some(&'@') {
        return None;
    }
    let query = characters[token_start + 1..cursor]
        .iter()
        .collect::<String>();
    if query.chars().count() > 128 || query.contains('@') {
        return None;
    }
    Some(AssistantMentionQuery {
        range: token_start..cursor,
        text: query,
    })
}

/// 在模型实际消息末尾附加 Argus 生成的来源元数据；界面仍只展示用户原始问题。
fn assistant_message_with_mentions(
    content: &str,
    selected_mentions: &[AssistantSelectedMention],
) -> String {
    let metadata = selected_mentions
        .iter()
        .filter(|selected| content.contains(&selected.marker))
        .map(|selected| {
            let candidate = &selected.candidate;
            serde_json::json!({
                "kind": candidate.kind.as_str(),
                "display_path": candidate.display_path,
                "path_prefix": (candidate.kind == AssistantMentionKind::Folder)
                    .then_some(candidate.display_path.as_str()),
                "source_ref": candidate.source_ref,
                "matching_source_count": candidate.matching_source_count,
            })
        })
        .collect::<Vec<_>>();
    if metadata.is_empty() {
        return content.to_string();
    }
    let serialized = serde_json::to_string(&metadata).unwrap_or_else(|_| "[]".to_string());
    format!(
        "{content}\n\n<ARGUS_SELECTED_SOURCES app_generated=\"true\">\n{serialized}\n</ARGUS_SELECTED_SOURCES>"
    )
}

impl Drop for AssistantPanel {
    /// 主窗口销毁时停止不可见的扫描、模型和日志工具任务。
    fn drop(&mut self) {
        self.cancel_background_work();
    }
}

impl Render for AssistantPanel {
    /// 渲染助手头部、虚拟消息流、状态信息和底部悬浮输入框。
    ///
    /// 未加载日志时不渲染任何会话内容，只提示先加载日志；会话实体随日志加载自动
    /// 销毁重建（见 `ArgusApp::reset_assistant_after_log_reload`）。
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.has_loaded_sources {
            let theme = self.theme.clone();
            return div()
                .id("assistant-panel-root")
                .size_full()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .overflow_hidden()
                .bg(rgb(theme.side_bar))
                .font_family(ARGUS_UI_FONT_FAMILY)
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("请先加载日志"),
                )
                .child(
                    div()
                        .mt_2()
                        .text_size(px(11.0))
                        .text_color(rgb(theme.foreground_muted))
                        .child("Agent 助手会话随日志加载自动创建，加载完成后即可提问。"),
                );
        }
        let entity = cx.entity();
        if !self.has_registered_scroll_handler {
            // 滚动监听闭包存放在面板自有的 ListState 内，必须持有弱引用；
            // 否则形成强引用环，日志重载销毁面板实体时永远无法释放并执行 Drop 取消后台任务。
            let scroll_entity = entity.downgrade();
            self.message_list
                .set_scroll_handler(move |event, _, app_cx| {
                    let _ = scroll_entity.update(app_cx, |panel, _| {
                        panel.is_following_latest = !event.is_scrolled;
                    });
                });
            self.has_registered_scroll_handler = true;
        }
        let native_entity = entity.clone();
        let native_input = NativeInput::new(self.input_focus.clone(), move |edit, _, app_cx| {
            native_entity.update(app_cx, |panel, panel_cx| {
                panel.input.apply_native_edit(&edit);
                panel.error = None;
                panel.refresh_mention_picker();
                panel_cx.notify();
            });
        });
        let unavailable_reason = self.unavailable_reason();
        let is_busy = self.status.is_busy();
        let can_stop = is_busy && self.status != AssistantPanelStatus::Cancelling;
        let can_send =
            !is_busy && unavailable_reason.is_none() && !self.input.value.trim().is_empty();
        let model_name = self
            .selected_model()
            .map(|model| model.name.clone())
            .unwrap_or_else(|| "未配置模型".to_string());
        let context_window = self
            .selected_model()
            .map(|model| model.context_window_tokens)
            .unwrap_or_default();
        let theme = self.theme.clone();
        let messages = self.messages.clone();
        let render_messages = messages.clone();
        let render_theme = theme.clone();
        let render_app = self.app.clone();
        let render_scope = self.scope.clone();
        let render_entity = entity.clone();
        let is_active = self.status == AssistantPanelStatus::Running;
        let last_index = messages.len().saturating_sub(1);
        let model_entity = entity.clone();
        let key_entity = entity.clone();
        let click_entity = entity.clone();
        let pointer_entity = entity.clone();
        let composer_action_entity = entity.clone();
        let mention_entity = entity.clone();
        let settings_app = self.app.clone();
        let jump_entity = entity.clone();
        let show_jump = !self.is_following_latest;

        div()
            .id("assistant-panel-root")
            .size_full()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .overflow_hidden()
            // 与日志来源树使用相同侧栏色块，不再通过左边框分割主内容和 Agent 区域。
            .bg(rgb(theme.side_bar))
            .font_family(ARGUS_UI_FONT_FAMILY)
            .child(
                div()
                    .h(px(48.0))
                    .flex_none()
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(render_icon(ArgusIcon::SmartAnalysis, theme.info, 16.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Agent 助手"),
                            )
                            .child(
                                div()
                                    .text_size(px(10.0))
                                    .text_color(rgb(theme.foreground_muted))
                                    .child(self.status.label()),
                            ),
                    ),
            )
            .when_some(unavailable_reason.clone(), |this, reason| {
                this.child(
                    div()
                        .mx_3()
                        .mt_3()
                        .p_3()
                        .rounded_lg()
                        .border_1()
                        .border_color(rgb(theme.border))
                        .bg(rgb(theme.content))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(rgb(theme.warning))
                                .child(reason),
                        )
                        .child(
                            div()
                                .id("assistant-open-model-settings")
                                .mt_2()
                                .text_size(px(10.0))
                                .text_color(rgb(theme.info))
                                .cursor_pointer()
                                .hover(|hover| hover.opacity(0.8))
                                .on_click(move |_, _, app_cx| {
                                    settings_app.update(app_cx, |app, settings_cx| {
                                        app.open_assistant_model_settings(settings_cx);
                                        settings_cx.notify();
                                    });
                                })
                                .child("打开模型配置"),
                        ),
                )
            })
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_hidden()
                    .child(
                        list(self.message_list.clone(), move |index, _, _| {
                            render_assistant_message(
                                render_messages.get(index),
                                index,
                                is_active && index == last_index,
                                render_app.clone(),
                                render_scope.clone(),
                                render_entity.clone(),
                                &render_theme,
                            )
                        })
                        .size_full(),
                    )
                    .child(render_assistant_scrollbar(
                        self.message_list.clone(),
                        entity.clone(),
                        &theme,
                    ))
                    .when(show_jump, |this| {
                        this.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom(px(8.0))
                                .flex()
                                .justify_center()
                                .child(
                                    div()
                                        .p_1()
                                        .rounded_full()
                                        .border_1()
                                        .border_color(rgb(theme.border))
                                        .bg(rgb(theme.content))
                                        .child(render_round_icon_button(
                                            "assistant-jump-latest",
                                            ArgusIcon::ArrowDown,
                                            "跳转到最新消息",
                                            false,
                                            IconButtonSize::Tiny,
                                            &theme,
                                            move |_, _, app_cx| {
                                                jump_entity.update(app_cx, |panel, panel_cx| {
                                                    panel.jump_to_latest();
                                                    panel_cx.notify();
                                                });
                                            },
                                        )),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .p_3()
                    .pt_2()
                    .child(
                        div()
                            .relative()
                            .when(self.mention_query.is_some(), |this| {
                                this.child(render_assistant_mention_picker(
                                    self.mention_candidates.clone(),
                                    self.mention_results.clone(),
                                    self.mention_highlighted_index,
                                    mention_entity,
                                    &theme,
                                ))
                            })
                            .child(render_textarea(
                                Textarea {
                                    id: "assistant-message-input",
                                    placeholder: "询问当前日志，输入 @ 添加文件或文件夹",
                                    value: self.input.value.clone(),
                                    is_disabled: self.status == AssistantPanelStatus::Cancelling
                                        || unavailable_reason.is_some(),
                                    is_focused: self.input.is_focused,
                                    cursor_index: self.input.cursor,
                                    selection_range: self.input.selection_range(),
                                    marked_range: self.input.marked_range.clone(),
                                    is_pointer_selecting: self.input.selection_drag.is_some(),
                                    visible_lines: 3,
                                    fill_height: false,
                                    scroll_handle: self.input_scroll.clone(),
                                    scroll_state: self.input_scroll_state.clone(),
                                    style: TextareaStyle::Composer,
                                    trailing_accessory: Some(InputAccessory {
                                        id: if is_busy {
                                            "assistant-stop"
                                        } else {
                                            "assistant-send"
                                        },
                                        icon: if is_busy {
                                            ArgusIcon::Stop
                                        } else {
                                            ArgusIcon::ArrowUp
                                        },
                                        tooltip: if is_busy {
                                            "停止当前任务"
                                        } else {
                                            "发送消息"
                                        },
                                    }),
                                    trailing_accessory_position:
                                        TextareaAccessoryPosition::BottomRight,
                                    trailing_accessory_always_visible: true,
                                    trailing_accessory_selected: if is_busy {
                                        can_stop
                                    } else {
                                        can_send
                                    },
                                    native_input: Some(native_input),
                                },
                                &theme,
                                move |event, _, app_cx| {
                                    app_cx.stop_propagation();
                                    key_entity.update(app_cx, |panel, panel_cx| {
                                        panel.handle_input_key(event, panel_cx);
                                        panel_cx.notify();
                                    });
                                },
                                move |_, window, app_cx| {
                                    app_cx.stop_propagation();
                                    click_entity.update(app_cx, |panel, panel_cx| {
                                        panel.input.is_focused = true;
                                        panel.input_focus.focus(window);
                                        panel_cx.notify();
                                    });
                                },
                                move |event: &InputPointerEvent, _, app_cx| {
                                    pointer_entity.update(app_cx, |panel, panel_cx| {
                                        match event.action {
                                            InputPointerAction::Begin => {
                                                panel.input.begin_pointer_selection(
                                                    event.character_index,
                                                    event.granularity,
                                                )
                                            }
                                            InputPointerAction::Extend => panel
                                                .input
                                                .update_pointer_selection(event.character_index),
                                            InputPointerAction::Finish => {
                                                panel.input.finish_pointer_selection()
                                            }
                                        }
                                        panel.refresh_mention_picker();
                                        panel_cx.notify();
                                    });
                                },
                                move |_, _, app_cx| {
                                    app_cx.stop_propagation();
                                    if is_busy && can_stop {
                                        composer_action_entity.update(app_cx, |panel, panel_cx| {
                                            panel.stop_current_work();
                                            panel_cx.notify();
                                        });
                                    } else if can_send {
                                        composer_action_entity.update(app_cx, |panel, panel_cx| {
                                            panel.submit_input(panel_cx);
                                            panel_cx.notify();
                                        });
                                    }
                                },
                            )),
                    )
                    .child(
                        div()
                            .mt_2()
                            .flex()
                            .items_end()
                            .gap_2()
                            .child(div().flex_1().min_w(px(0.0)).child(render_assistant_usage(
                                self.budget,
                                context_window,
                                self.error.as_deref(),
                                &theme,
                            )))
                            .child(
                                div()
                                    .id("assistant-model-selector")
                                    .max_w(px(160.0))
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .when(!self.status.is_busy() && self.models.len() > 1, |this| {
                                        this.hover(|hover| hover.bg(rgb(theme.current_line)))
                                            .on_click(move |_, _, app_cx| {
                                                model_entity.update(app_cx, |panel, panel_cx| {
                                                    panel.select_next_model();
                                                    panel_cx.notify();
                                                });
                                            })
                                    })
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(10.0))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(model_name),
                                    ),
                            ),
                    ),
            )
    }
}

/// 渲染输入框上方的“@”来源候选；只展示快照内路径，不暴露真实本地位置。
fn render_assistant_mention_picker(
    candidates: Arc<Vec<AssistantMentionCandidate>>,
    result_indices: Vec<usize>,
    highlighted_index: usize,
    panel: Entity<AssistantPanel>,
    theme: &AppTheme,
) -> AnyElement {
    let rows = result_indices
        .into_iter()
        .enumerate()
        .filter_map(|(row_index, candidate_index)| {
            let candidate = candidates.get(candidate_index)?.clone();
            let row_panel = panel.clone();
            Some(
                div()
                    .id(("assistant-mention-candidate", candidate_index))
                    .h(px(34.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .when(row_index == highlighted_index, |this| {
                        this.bg(rgb(theme.selection))
                    })
                    .hover(|this| this.bg(rgb(theme.current_line)))
                    .child(render_icon(
                        if candidate.kind == AssistantMentionKind::Folder {
                            ArgusIcon::Folder
                        } else {
                            ArgusIcon::FileText
                        },
                        theme.foreground_muted,
                        13.0,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .truncate()
                            .text_size(px(10.0))
                            .child(candidate.display_path),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(9.0))
                            .text_color(rgb(theme.foreground_muted))
                            .child(if candidate.kind == AssistantMentionKind::Folder {
                                format!("{} 个日志", candidate.matching_source_count)
                            } else {
                                "文件".to_string()
                            }),
                    )
                    .on_click(move |_, _, app_cx| {
                        app_cx.stop_propagation();
                        row_panel.update(app_cx, |assistant, assistant_cx| {
                            if let Some(query) = assistant.mention_query.clone() {
                                assistant.select_mention(query, candidate_index);
                            }
                            assistant_cx.notify();
                        });
                    })
                    .into_any_element(),
            )
        })
        .collect::<Vec<_>>();
    div()
        .id("assistant-mention-picker")
        .mb_2()
        .rounded_lg()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.content))
        .overflow_hidden()
        .child(
            div()
                .h(px(28.0))
                .px_2()
                .flex()
                .items_center()
                .text_size(px(9.0))
                .text_color(rgb(theme.foreground_muted))
                .child("选择当前日志来源中的文件或文件夹"),
        )
        .when(rows.is_empty(), |this| {
            this.child(
                div()
                    .px_2()
                    .pb_2()
                    .text_size(px(10.0))
                    .text_color(rgb(theme.foreground_muted))
                    .child(if candidates.is_empty() {
                        "正在准备完整日志来源…"
                    } else {
                        "没有匹配的文件或文件夹"
                    }),
            )
        })
        .children(rows)
        .into_any_element()
}

/// 渲染一条虚拟消息；屏幕外消息不会进入当前布局树。
fn render_assistant_message(
    message: Option<&AssistantPanelMessage>,
    index: usize,
    is_active: bool,
    app: Entity<ArgusApp>,
    scope: Option<Arc<SourceScopeSnapshot>>,
    panel: Entity<AssistantPanel>,
    theme: &AppTheme,
) -> AnyElement {
    let Some(message) = message else {
        return div().into_any_element();
    };
    match message {
        AssistantPanelMessage::User(message) => {
            render_user_message(message, theme).into_any_element()
        }
        AssistantPanelMessage::Trace(trace) => {
            render_trace_message(trace, index, is_active, theme).into_any_element()
        }
        AssistantPanelMessage::ToolGroup {
            traces,
            is_expanded,
        } => render_tool_group(traces, *is_expanded, index, is_active, panel, theme)
            .into_any_element(),
        AssistantPanelMessage::Reasoning(content) => render_model_message(
            "思考过程",
            content,
            true,
            is_active,
            index,
            Vec::new(),
            app,
            scope,
            theme,
        )
        .into_any_element(),
        AssistantPanelMessage::Answer { content, citations } => render_model_message(
            "AI 回答",
            content,
            false,
            is_active,
            index,
            citations.clone(),
            app,
            scope,
            theme,
        )
        .into_any_element(),
    }
}

/// 渲染用户消息和排队状态。
fn render_user_message(message: &AgentUserMessage, theme: &AppTheme) -> impl IntoElement {
    let status = match message.status {
        AgentUserMessageStatus::Queued => "排队中",
        AgentUserMessageStatus::Consumed => "",
        AgentUserMessageStatus::Rejected => "未发送",
    };
    div()
        .w_full()
        .px_4()
        .py_3()
        .flex()
        .gap_3()
        .child(div().w(px(18.0)).flex_none().pt(px(2.0)).child(render_icon(
            ArgusIcon::ArrowRight,
            theme.info,
            13.0,
        )))
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
                                .text_size(px(11.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("你"),
                        )
                        .when(!status.is_empty(), |this| {
                            this.child(
                                div()
                                    .text_size(px(9.0))
                                    .text_color(rgb(theme.foreground_muted))
                                    .child(status),
                            )
                        }),
                )
                .child(div().mt_1().child(render_markdown(
                    &message.content,
                    MarkdownStyle {
                        font_size: 12.0,
                        line_height: 19.0,
                        color: theme.foreground,
                    },
                    theme,
                ))),
        )
}

/// 渲染思考或最终回答，并在回答下方展示可信可点击引用。
#[allow(clippy::too_many_arguments)]
fn render_model_message(
    title: &'static str,
    content: &str,
    is_reasoning: bool,
    is_active: bool,
    index: usize,
    citations: Vec<AssistantCitation>,
    app: Entity<ArgusApp>,
    scope: Option<Arc<SourceScopeSnapshot>>,
    theme: &AppTheme,
) -> impl IntoElement {
    let leading = if is_active {
        render_loading_spinner(("assistant-message-loading", index), theme.info, 13.0)
    } else {
        render_icon(
            ArgusIcon::SmartAnalysis,
            if is_reasoning {
                theme.foreground_muted
            } else {
                theme.info
            },
            13.0,
        )
        .into_any_element()
    };
    let color = if is_reasoning {
        theme.foreground_muted
    } else {
        theme.foreground
    };
    let citation_elements = citations
        .iter()
        .enumerate()
        .map(|(citation_index, citation)| {
            render_assistant_citation(citation_index, citation, app.clone(), scope.clone(), theme)
        })
        .collect::<Vec<_>>();
    div()
        .w_full()
        .px_4()
        .py_3()
        .flex()
        .gap_3()
        .child(div().w(px(18.0)).flex_none().pt(px(2.0)).child(leading))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .child(
                    div()
                        .text_size(px(11.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(if is_reasoning {
                            theme.foreground_muted
                        } else {
                            theme.info
                        }))
                        .child(title),
                )
                .child(div().mt_1().child(render_markdown(
                    content,
                    MarkdownStyle {
                        font_size: 12.0,
                        line_height: 19.0,
                        color,
                    },
                    theme,
                )))
                .children(citation_elements),
        )
}

/// 渲染一条可跳转的日志引用卡片。
fn render_assistant_citation(
    index: usize,
    citation: &AssistantCitation,
    app: Entity<ArgusApp>,
    scope: Option<Arc<SourceScopeSnapshot>>,
    theme: &AppTheme,
) -> AnyElement {
    let source = scope
        .as_ref()
        .and_then(|scope| scope.source(&citation.source_ref));
    let source_id = source.map(|source| source.source_id);
    let path = source
        .map(|source| source.relative_path.clone())
        .unwrap_or_else(|| "来源已失效".to_string());
    let start_line = citation.start_line;
    let lines = citation
        .display_excerpt
        .as_ref()
        .map(|excerpt| {
            excerpt
                .lines
                .iter()
                .map(|line| {
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .w(px(36.0))
                                .flex_none()
                                .text_right()
                                .text_color(rgb(theme.syntax.comment))
                                .child(line.line_number.to_string()),
                        )
                        .child(div().flex_1().min_w(px(0.0)).child(line.text.clone()))
                        .into_any_element()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    div()
        .id(("assistant-citation", index))
        .mt_3()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.content))
        .when(source_id.is_some(), |this| {
            let navigate_app = app.clone();
            this.cursor_pointer()
                .hover(|hover| hover.border_color(rgb(theme.info)))
                .on_click(move |_, _, app_cx| {
                    if let Some(source_id) = source_id {
                        navigate_app.update(app_cx, |main_app, cx| {
                            main_app.open_ai_evidence(source_id, start_line, cx);
                            cx.notify();
                        });
                    }
                })
        })
        .child(
            div()
                .text_size(px(10.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(if source_id.is_some() {
                    theme.info
                } else {
                    theme.foreground_muted
                }))
                .child(format!(
                    "[E{}] {} · 第 {}-{} 行",
                    index + 1,
                    path,
                    citation.start_line,
                    citation.end_line
                )),
        )
        .child(
            div()
                .mt_1()
                .text_size(px(10.0))
                .text_color(rgb(theme.foreground_muted))
                .child(citation.rationale.clone()),
        )
        .when(!lines.is_empty(), |this| {
            this.child(
                div()
                    .mt_2()
                    .pt_2()
                    .border_t_1()
                    .border_color(rgb(theme.border))
                    .font_family(ARGUS_LOG_FONT_FAMILY)
                    .text_size(px(9.0))
                    .line_height(px(16.0))
                    .children(lines),
            )
        })
        .into_any_element()
}

/// 渲染普通轻量轨迹。
fn render_trace_message(
    trace: &AgentTraceEntry,
    index: usize,
    is_active: bool,
    theme: &AppTheme,
) -> impl IntoElement {
    let color = if trace.kind == AgentTraceKind::Warning {
        theme.warning
    } else {
        theme.foreground_muted
    };
    let leading = if is_active {
        render_loading_spinner(("assistant-trace-loading", index), color, 13.0)
    } else {
        render_icon(trace_icon(trace.kind), color, 13.0).into_any_element()
    };
    div()
        .w_full()
        .px_4()
        .py_3()
        .flex()
        .gap_3()
        .child(div().w(px(18.0)).flex_none().pt(px(2.0)).child(leading))
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
                                .text_size(px(11.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(trace.title.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(9.0))
                                .text_color(rgb(theme.foreground_muted))
                                .opacity(0.55)
                                .child(trace.created_at.format("%H:%M:%S").to_string()),
                        ),
                )
                .child(
                    div()
                        .mt_1()
                        .text_size(px(10.0))
                        .line_height(px(17.0))
                        .text_color(rgb(color))
                        .child(trace.detail.clone()),
                ),
        )
}

/// 渲染连续工具组；悬停不添加整行灰色背景。
fn render_tool_group(
    traces: &[AgentTraceEntry],
    is_expanded: bool,
    index: usize,
    is_active: bool,
    panel: Entity<AssistantPanel>,
    theme: &AppTheme,
) -> impl IntoElement {
    let last = traces.last().expect("工具轨迹组不能为空");
    let children = if is_expanded {
        traces
            .iter()
            .map(|trace| {
                div()
                    .mt_2()
                    .pl_3()
                    .border_l_1()
                    .border_color(rgb(theme.border))
                    .child(div().text_size(px(10.0)).child(trace.title.clone()))
                    .child(
                        div()
                            .mt_1()
                            .text_size(px(9.0))
                            .text_color(rgb(theme.foreground_muted))
                            .child(trace.detail.clone()),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let toggle_panel = panel.clone();
    let leading = if is_active {
        render_loading_spinner(
            ("assistant-tool-loading", index),
            theme.foreground_muted,
            13.0,
        )
    } else {
        render_icon(ArgusIcon::Settings, theme.foreground_muted, 13.0).into_any_element()
    };
    div()
        .id(("assistant-tool-group", index))
        .w_full()
        .px_4()
        .py_3()
        .flex()
        .gap_3()
        .cursor_pointer()
        .on_click(move |_, _, app_cx| {
            toggle_panel.update(app_cx, |panel, panel_cx| {
                panel.toggle_tool_group(index);
                panel_cx.notify();
            });
        })
        .child(div().w(px(18.0)).flex_none().pt(px(2.0)).child(leading))
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
                                .text_size(px(11.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format!("工具 · {} 条", traces.len())),
                        )
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(rgb(theme.foreground_muted))
                                .child(last.title.clone()),
                        )
                        .child(render_icon(
                            if is_expanded {
                                ArgusIcon::Collapse
                            } else {
                                ArgusIcon::Expand
                            },
                            theme.foreground_muted,
                            12.0,
                        )),
                )
                .child(
                    div()
                        .mt_1()
                        .text_size(px(10.0))
                        .text_color(rgb(theme.foreground_muted))
                        .child(last.detail.clone()),
                )
                .children(children),
        )
}

/// 根据轨迹类型选择紧凑图标。
fn trace_icon(kind: AgentTraceKind) -> ArgusIcon {
    match kind {
        AgentTraceKind::Warning => ArgusIcon::Info,
        AgentTraceKind::Model
        | AgentTraceKind::Reasoning
        | AgentTraceKind::Output
        | AgentTraceKind::Report => ArgusIcon::SmartAnalysis,
        AgentTraceKind::Tool => ArgusIcon::Settings,
        AgentTraceKind::User => ArgusIcon::ArrowRight,
        AgentTraceKind::Status => ArgusIcon::Info,
    }
}

/// 渲染 Token 与上下文占用信息；错误优先显示且不进入消息历史。
fn render_assistant_usage(
    budget: AgentBudgetSnapshot,
    context_window: u64,
    error: Option<&str>,
    theme: &AppTheme,
) -> impl IntoElement {
    let total = if budget.total_tokens == 0 {
        budget.input_tokens.saturating_add(budget.output_tokens)
    } else {
        budget.total_tokens
    };
    let context = budget.latest_input_tokens.map_or_else(
        || "上下文 --".to_string(),
        |input| {
            if context_window == 0 {
                format!("上下文 {}", compact_tokens(input))
            } else {
                format!(
                    "上下文 {:.1}%",
                    input as f64 * 100.0 / context_window as f64
                )
            }
        },
    );
    div()
        .px_1()
        .flex()
        .items_center()
        .gap_2()
        .text_size(px(9.0))
        .text_color(rgb(if error.is_some() {
            theme.error
        } else {
            theme.foreground_muted
        }))
        .child(
            error
                .map(str::to_string)
                .unwrap_or_else(|| format!("Token {} · {}", compact_tokens(total), context)),
        )
}

/// 紧凑显示 Token 数量。
fn compact_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

/// 绘制无轨道消息滚动条并支持拖动。
fn render_assistant_scrollbar(
    list_state: ListState,
    panel: Entity<AssistantPanel>,
    theme: &AppTheme,
) -> AnyElement {
    let viewport = list_state.viewport_bounds();
    let max_scroll = list_state.max_offset_for_scrollbar().height;
    let current_scroll = -list_state.scroll_px_offset_for_scrollbar().y;
    let content_height = viewport.size.height + max_scroll;
    let Some(metrics) = scrollbar_metrics(
        viewport.size.height,
        content_height,
        current_scroll,
        ASSISTANT_SCROLLBAR_PADDING,
        ASSISTANT_SCROLLBAR_MIN_THUMB,
    ) else {
        return canvas(
            |_, _, _| (),
            move |_, _, _, app_cx: &mut App| {
                if list_state.viewport_bounds().size.height > px(0.0)
                    && list_state.max_offset_for_scrollbar().height > px(0.0)
                {
                    app_cx.notify(panel.entity_id());
                }
            },
        )
        .absolute()
        .size_full()
        .into_any_element();
    };
    let mouse_state = list_state.clone();
    div()
        .id("assistant-message-scrollbar")
        .absolute()
        .top(metrics.thumb_start)
        .right(px(ASSISTANT_SCROLLBAR_PADDING))
        .w(px(ASSISTANT_SCROLLBAR_WIDTH))
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
                        let panel = panel.clone();
                        let list_state = mouse_state.clone();
                        move |event: &MouseDownEvent, phase, _, app_cx| {
                            if !phase.bubble()
                                || event.button != MouseButton::Left
                                || !thumb_bounds.contains(&event.position)
                            {
                                return;
                            }
                            list_state.scrollbar_drag_started();
                            panel.update(app_cx, |view, _| {
                                view.scrollbar_drag_offset =
                                    Some(event.position.y - thumb_bounds.top());
                            });
                            app_cx.stop_propagation();
                        }
                    });
                    window.on_mouse_event({
                        let panel = panel.clone();
                        let list_state = mouse_state.clone();
                        move |event: &MouseUpEvent, phase, _, app_cx| {
                            if !phase.bubble() || event.button != MouseButton::Left {
                                return;
                            }
                            let handled = panel.update(app_cx, |view, _| {
                                view.scrollbar_drag_offset.take().is_some()
                            });
                            if handled {
                                list_state.scrollbar_drag_ended();
                                app_cx.stop_propagation();
                            }
                        }
                    });
                    window.on_mouse_event({
                        let panel = panel.clone();
                        let list_state = mouse_state.clone();
                        move |event: &MouseMoveEvent, phase, _, app_cx| {
                            if !phase.bubble() || !event.dragging() {
                                return;
                            }
                            let Some(cursor_offset) = panel.read(app_cx).scrollbar_drag_offset
                            else {
                                return;
                            };
                            let pointer = event.position.y - viewport.top();
                            let scroll =
                                scrollbar_scroll_for_drag(pointer, cursor_offset, &metrics);
                            list_state.set_offset_from_scrollbar(point(px(0.0), -scroll));
                            panel.update(app_cx, |view, _| {
                                view.is_following_latest = metrics.max_scroll - scroll <= px(0.5);
                            });
                            app_cx.stop_propagation();
                        }
                    });
                },
            )
            .size_full(),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigManager;
    use crate::config::paths::isolated_test_dir;
    use gpui::{AppContext, TestAppContext};

    /// 验证“@”查询使用 Unicode 字符范围，并且邮箱式正文不会误触发来源候选。
    #[test]
    fn mention_query_only_matches_current_at_token() {
        let input = TextInputState::from_value("检查一下 @memory".to_string());
        let query = active_mention_query(&input).expect("独立 @ token 应触发候选");
        assert_eq!(query.text, "memory");
        assert_eq!(query.range, 5..12);

        let email = TextInputState::from_value("联系 user@example.com".to_string());
        assert!(active_mention_query(&email).is_none());
    }

    /// 验证只有仍出现在可见正文中的已选来源才会生成模型侧可信绑定。
    #[test]
    fn selected_mentions_are_hidden_and_bound_to_runtime_message() {
        let selected = AssistantSelectedMention {
            marker: "@来源/memory.log".to_string(),
            candidate: AssistantMentionCandidate {
                display_path: "来源/memory.log".to_string(),
                search_key: "来源/memory.log".to_string(),
                kind: AssistantMentionKind::File,
                source_ref: Some("opaque-ref".to_string()),
                matching_source_count: 1,
            },
        };
        let runtime = assistant_message_with_mentions(
            "检查 @来源/memory.log 是否存在 OOM",
            std::slice::from_ref(&selected),
        );
        assert!(runtime.contains("ARGUS_SELECTED_SOURCES"));
        assert!(runtime.contains("opaque-ref"));
        assert_eq!(
            assistant_message_with_mentions("检查全部日志", &[selected]),
            "检查全部日志"
        );
    }

    /// 验证普通来源补齐和分析配置更新保留对话；全量重载的新建会话语义由 app 层实体重建覆盖。
    #[gpui::test]
    fn incremental_source_updates_preserve_assistant_context(cx: &mut TestAppContext) {
        let directory = isolated_test_dir("assistant-context-lifecycle");
        let manager = ConfigManager::new(directory.join("settings.toml"));
        let app = cx.new(|_| ArgusApp::new_with_config_manager(manager));
        let panel = cx.new(|panel_cx| {
            AssistantPanel::new(app, AppTheme::dark(), AiConfig::default(), false, panel_cx)
        });

        panel.update(cx, |panel, panel_cx| {
            panel.push_trace(AgentTraceEntry::new(
                AgentTraceKind::Status,
                "已有对话",
                "应在非替换式来源更新后继续保留",
            ));
            let original_message_count = panel.messages.len();

            panel.accept_source_registry_revision(1, false, panel_cx);
            assert_eq!(panel.messages.len(), original_message_count);

            panel.invalidate_scope_for_analysis_configuration_change(1, "日志说明已更新", panel_cx);
            assert!(panel.messages.len() > original_message_count);
        });
    }

    /// 验证当前轮没有流式正文时，最终回答会新增消息而不会覆盖上一轮答案。
    #[gpui::test]
    fn final_answer_without_stream_delta_does_not_overwrite_previous_turn(cx: &mut TestAppContext) {
        let directory = isolated_test_dir("assistant-final-answer-boundary");
        let manager = ConfigManager::new(directory.join("settings.toml"));
        let app = cx.new(|_| ArgusApp::new_with_config_manager(manager));
        let panel = cx.new(|panel_cx| {
            AssistantPanel::new(app, AppTheme::dark(), AiConfig::default(), false, panel_cx)
        });

        panel.update(cx, |panel, _panel_cx| {
            let mut current_question = AgentUserMessage::queued("当前问题".to_string());
            current_question.status = AgentUserMessageStatus::Consumed;
            panel.replace_messages(vec![
                AssistantPanelMessage::Answer {
                    content: "上一轮答案".to_string(),
                    citations: Vec::new(),
                },
                AssistantPanelMessage::User(current_question),
            ]);
            panel.active_turn_message_start = Some(2);

            panel.finish_answer("当前轮答案".to_string(), Vec::new());

            assert!(matches!(
                panel.messages.first(),
                Some(AssistantPanelMessage::Answer { content, .. }) if content == "上一轮答案"
            ));
            assert!(matches!(
                panel.messages.get(2),
                Some(AssistantPanelMessage::Answer { content, .. }) if content == "当前轮答案"
            ));
        });
    }

    /// 验证分析配置变化会重新排队已消费问题，并移除旧配置产生的半截回答。
    #[gpui::test]
    fn configuration_change_requeues_active_question_and_drops_partial_answer(
        cx: &mut TestAppContext,
    ) {
        let directory = isolated_test_dir("assistant-configuration-requeue");
        let manager = ConfigManager::new(directory.join("settings.toml"));
        let app = cx.new(|_| ArgusApp::new_with_config_manager(manager));
        let panel = cx.new(|panel_cx| {
            AssistantPanel::new(app, AppTheme::dark(), AiConfig::default(), false, panel_cx)
        });

        panel.update(cx, |panel, panel_cx| {
            let mut question = AgentUserMessage::queued("重新分析当前问题".to_string());
            question.status = AgentUserMessageStatus::Consumed;
            let message_id = question.message_id.clone();
            panel.replace_messages(vec![
                AssistantPanelMessage::User(question),
                AssistantPanelMessage::Reasoning("旧配置思考".to_string()),
                AssistantPanelMessage::Answer {
                    content: "旧配置半截答案".to_string(),
                    citations: Vec::new(),
                },
            ]);
            panel.status = AssistantPanelStatus::Running;
            panel.active_turn_message_start = Some(1);
            panel.active_turn_user_message_ids = vec![message_id];
            panel.turn_cancellation = Some(tokio_util::sync::CancellationToken::new());

            panel.invalidate_scope_for_analysis_configuration_change(2, "日志说明已更新", panel_cx);

            assert!(matches!(
                panel.messages.first(),
                Some(AssistantPanelMessage::User(message))
                    if message.status == AgentUserMessageStatus::Queued
            ));
            assert!(!panel.messages.iter().any(|message| matches!(
                message,
                AssistantPanelMessage::Reasoning(_) | AssistantPanelMessage::Answer { .. }
            )));
            assert_eq!(panel.status, AssistantPanelStatus::Idle);
            assert!(panel.active_turn_user_message_ids.is_empty());
        });
    }

    /// 验证运行中补充的隐藏来源绑定会保留到模型确认消费后，而不是在通道发送时提前删除。
    #[gpui::test]
    fn queued_runtime_binding_is_kept_until_turn_consumes_message(cx: &mut TestAppContext) {
        let directory = isolated_test_dir("assistant-runtime-binding-lifecycle");
        let manager = ConfigManager::new(directory.join("settings.toml"));
        let app = cx.new(|_| ArgusApp::new_with_config_manager(manager));
        let panel = cx.new(|panel_cx| {
            AssistantPanel::new(app, AppTheme::dark(), AiConfig::default(), false, panel_cx)
        });

        panel.update(cx, |panel, _panel_cx| {
            let message = AgentUserMessage::queued("检查指定来源".to_string());
            let message_id = message.message_id.clone();
            let runtime_content =
                "检查指定来源\n<ARGUS_SELECTED_SOURCES>opaque-ref</ARGUS_SELECTED_SOURCES>";
            let (sender, receiver) = async_channel::bounded(1);
            panel.user_message_sender = Some(sender);
            panel
                .pending_runtime_messages
                .insert(message_id.clone(), runtime_content.to_string());

            panel.queue_running_message(message);

            let delivered = receiver.try_recv().expect("补充消息应进入当前回答通道");
            assert_eq!(delivered.content, runtime_content);
            assert!(panel.pending_runtime_messages.contains_key(&message_id));

            panel.active_turn_user_message_ids.push(message_id.clone());
            panel.finish_active_turn();
            assert!(!panel.pending_runtime_messages.contains_key(&message_id));
        });
    }
}
