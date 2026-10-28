//! 文件职责：渲染并管理标签页中的可持续交互智能助手会话。
//! 创建日期：2026-07-16
//! 修改日期：2026-10-28
//! 作者：Argus 开发团队
//! 主要功能：按需固化全来源范围、多轮会话、模型切换、流式 Markdown、工具轨迹分组、Token 状态和可信日志引用跳转。

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, FocusHandle, FontWeight, IntoElement,
    KeyDownEvent, ListAlignment, ListState, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Render, ScrollHandle, Subscription, Timer, WeakEntity, Window, canvas,
    div, list, point, prelude::*, px, rgb,
};

use crate::agent::{
    AgentBudgetSnapshot, AgentEvent, AgentSessionStatus, AgentStreamKind, AgentTraceEntry,
    AgentTraceKind, AgentUserMessage, AgentUserMessageStatus, AssistantHistoryTurn,
    AssistantRunRequest, BashApprovalDecision, SourceScopeSnapshot, agent_runtime, load_api_key,
    prepare_agent_source_scope, run_assistant_turn,
};
use crate::app::{ArgusApp, TextInputState, observe_app_theme};
use crate::config::{AiConfig, AiModelProfile};
use crate::fonts::ARGUS_UI_FONT_FAMILY;
use crate::infra::text_selection::{
    character_count, replace_character_range, slice_character_range,
};
use crate::theme::AppTheme;
use crate::ui::components::bash_approval::{
    BashApprovalStatus, bash_approval_reason_label, render_bash_approval_card,
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
use crate::ui::components::markdown::{MarkdownStyle, markdown_plain_text, render_markdown};
use crate::ui::components::scrollbar::{
    scroll_area_hover_handler, scrollbar_metrics, scrollbar_scroll_for_drag,
    with_scrollbar_visibility,
};

/// 会话正文与输入区的最大阅读宽度。
const ASSISTANT_CONTENT_MAX_WIDTH: f32 = 860.0;
/// “@”文件候选单次展示数量，限制弹出层高度。
const ASSISTANT_MENTION_RESULT_LIMIT: usize = 8;
/// 页面流式事件合并窗口，限制高吞吐模型触发的重绘频率。
const ASSISTANT_EVENT_BATCH_INTERVAL: Duration = Duration::from_millis(16);
/// 单条用户消息边界；避免异常输入无限扩张内存和模型请求。
const ASSISTANT_MESSAGE_MAX_BYTES: usize = 32 * 1024;
/// 虚拟消息列表上下预渲染高度。
const ASSISTANT_LIST_OVERDRAW: f32 = 240.0;
/// 消息滚动条滑块宽度，不绘制轨道。
const ASSISTANT_SCROLLBAR_WIDTH: f32 = 5.0;
/// 消息滚动条上下留白。
const ASSISTANT_SCROLLBAR_PADDING: f32 = 4.0;
/// 消息滚动条最小滑块高度。
const ASSISTANT_SCROLLBAR_MIN_THUMB: f32 = 32.0;
/// 智能助手输入中可通过“@”选择的来源类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AssistantMentionKind {
    /// 目录；模型可用路径前缀定位其日志后代。
    Folder,
    /// 可读取文件。
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

/// 从来源快照构建的安全“@”候选；只包含工作区相对路径。
#[derive(Clone, Debug)]
struct AssistantMentionCandidate {
    /// 文件或目录在工作区中的展示路径。
    display_path: String,
    /// 预计算小写搜索文本。
    search_key: String,
    /// 候选类型。
    kind: AssistantMentionKind,
    /// 目录内日志后代数量；文件固定为 1。
    matching_source_count: usize,
}

/// 当前输入已经选择的候选；仅当标记仍在正文中时随消息发送。
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

/// 智能助手当前运行状态；每轮完成后回到空闲，会话和对话历史继续保留。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AssistantTabStatus {
    Idle,
    Running,
    Cancelling,
}

impl AssistantTabStatus {
    /// 返回头部紧凑状态文案。
    fn label(self) -> &'static str {
        match self {
            Self::Idle => "就绪",
            Self::Running => "回答中",
            Self::Cancelling => "正在停止",
        }
    }

    /// 返回当前是否存在可主动停止的后台工作。
    fn is_busy(self) -> bool {
        matches!(self, Self::Running | Self::Cancelling)
    }
}

/// 虚拟消息流中的稳定业务条目。
#[derive(Clone, Debug)]
enum AssistantTabMessage {
    /// 用户输入；运行中补充会经历排队和消费状态变化。
    User(AgentUserMessage),
    /// 普通状态、模型、警告轨迹。
    Trace(AgentTraceEntry),
    /// 连续工具调用组；默认只展示最后一条。
    ToolGroup {
        traces: Vec<AgentTraceEntry>,
        is_expanded: bool,
    },
    /// 一条 bash 审批确认卡片。
    BashApproval {
        /// 审批请求标识。
        request_id: String,
        /// 待审批的完整命令。
        command: String,
        /// 当前审批状态。
        status: BashApprovalStatus,
    },
    /// 模型思考过程，仅在当前内存会话展示。
    Reasoning(String),
    /// 模型最终可见回答。
    Answer { content: String },
}

/// 主内容区中的智能助手页签实体。
pub(crate) struct AssistantTab {
    /// 主应用弱引用，避免应用与页签实体形成强引用环。
    app: WeakEntity<ArgusApp>,
    /// 当前主题快照。
    theme: AppTheme,
    /// 已启用模型快照；由主应用在创建页签或保存设置时主动注入。
    models: Vec<AiModelProfile>,
    /// 当前选择模型的稳定配置 ID。
    selected_model_id: Option<String>,
    /// 当前模型最近一次系统凭据检查结果。
    credential_error: Option<String>,
    /// 当前轮次使用的全来源快照；每次发送前重新固化。
    scope: Option<Arc<SourceScopeSnapshot>>,
    /// 当前快照可选择的文件和目录候选。
    mention_candidates: Arc<Vec<AssistantMentionCandidate>>,
    /// 当前“@”查询；为空时不展示候选面板。
    mention_query: Option<AssistantMentionQuery>,
    /// 当前查询命中的候选下标。
    mention_results: Vec<usize>,
    /// 键盘当前高亮的候选行。
    mention_highlighted_index: usize,
    /// 当前编辑消息已选择的可信来源。
    selected_mentions: Vec<AssistantSelectedMention>,
    /// 可见消息 ID 到模型实际消息的映射，用于隐藏 Argus 生成的来源元数据。
    pending_runtime_messages: HashMap<String, String>,
    /// 已完成的跨 Provider 中立历史。
    history: Vec<AssistantHistoryTurn>,
    /// 当前消息流；使用 Arc 让虚拟列表渲染闭包只做轻量克隆。
    messages: Arc<Vec<AssistantTabMessage>>,
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
    /// 页面根焦点句柄，点击输入框以外区域时承接焦点，让输入框真正失焦。
    root_focus: FocusHandle,
    /// 当前状态。
    status: AssistantTabStatus,
    /// 最近一次可展示错误。
    error: Option<String>,
    /// 当前回答 Token 用量。
    budget: AgentBudgetSnapshot,
    /// 当前回答追加消息发送端。
    user_message_sender: Option<async_channel::Sender<AgentUserMessage>>,
    /// 当前回答 bash 审批答复发送端。
    bash_decision_sender: Option<async_channel::Sender<BashApprovalDecision>>,
    /// 当前回答未消费消息计数。
    pending_user_messages: Arc<AtomicUsize>,
    /// 当前回答取消令牌。
    turn_cancellation: Option<tokio_util::sync::CancellationToken>,
    /// 回答 generation，拒绝上一轮事件污染新回答。
    turn_generation: u64,
    /// 当前回答开始追加流式消息的位置，用于自动重试时精确移除未完成正文。
    active_turn_message_start: Option<usize>,
    /// 当前回答已经实际交给模型的用户消息 ID。
    active_turn_user_message_ids: Vec<String>,
    /// 是否处于“选择复制”文本模式。
    is_text_selection_mode: bool,
    /// 选择复制模式下的全文输入状态。
    selection_text: TextInputState,
    /// 选择复制模式的焦点句柄。
    selection_focus: FocusHandle,
    /// 选择复制全文的滚动句柄。
    selection_scroll: ScrollHandle,
    /// 选择复制全文的自绘滚动状态。
    selection_scroll_state: TextareaScrollState,
    /// 主题观察订阅。
    _theme_observer: Subscription,
}

impl AssistantTab {
    /// 创建空闲、内存态且尚未扫描来源的智能助手。
    ///
    /// `ArgusApp` 会在自身更新事务中延迟创建本实体，因此构造过程不能通过
    /// `app.read(cx)` 回读父实体，否则 GPUI 会因同一实体重入借用而直接 panic。
    pub(crate) fn new(
        app: Entity<ArgusApp>,
        theme: AppTheme,
        ai_config: AiConfig,
        cx: &mut Context<Self>,
    ) -> Self {
        let _theme_observer = observe_app_theme(cx, &app, theme.clone(), |panel, theme, _| {
            panel.theme = theme.clone();
        });
        let welcome = AssistantTabMessage::Trace(AgentTraceEntry::new(
            AgentTraceKind::Status,
            "智能助手",
            "输入问题后固化当前工作区清单，并按需调用工作目录内的只读工具；非白名单命令需要你确认。",
        ));
        let mut panel = Self {
            app: app.downgrade(),
            theme,
            models: Vec::new(),
            selected_model_id: None,
            credential_error: None,
            scope: None,
            mention_candidates: Arc::new(Vec::new()),
            mention_query: None,
            mention_results: Vec::new(),
            mention_highlighted_index: 0,
            selected_mentions: Vec::new(),
            pending_runtime_messages: HashMap::new(),
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
            root_focus: cx.focus_handle(),
            status: AssistantTabStatus::Idle,
            error: None,
            budget: AgentBudgetSnapshot::default(),
            user_message_sender: None,
            bash_decision_sender: None,
            pending_user_messages: Arc::new(AtomicUsize::new(0)),
            turn_cancellation: None,
            turn_generation: 0,
            active_turn_message_start: None,
            active_turn_user_message_ids: Vec::new(),
            is_text_selection_mode: false,
            selection_text: TextInputState::default(),
            selection_focus: cx.focus_handle(),
            selection_scroll: ScrollHandle::new(),
            selection_scroll_state: TextareaScrollState::new(),
            _theme_observer,
        };
        panel.apply_model_configuration(ai_config);
        panel
    }

    /// 应用注入的模型配置，保留仍有效的选择并重新检查对应系统凭据。
    ///
    /// 配置通过值传递，避免页签构造或渲染期间读取父 `ArgusApp` 造成实体重入。
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

    /// 返回当前不可发送的产品原因；来源范围在发送时再校验。
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
        if let Some(reason) = self.unavailable_reason() {
            self.error = Some(reason);
            return;
        }
        if self.status == AssistantTabStatus::Idle
            && let Err(error) = self.refresh_scope_from_app(cx)
        {
            self.error = Some(error);
            return;
        }

        let runtime_content = assistant_message_with_mentions(&content, &self.selected_mentions);
        let message = AgentUserMessage::queued(content);
        self.pending_runtime_messages
            .insert(message.message_id.clone(), runtime_content);
        self.push_message(AssistantTabMessage::User(message.clone()));
        self.input = TextInputState::default();
        self.input.is_focused = true;
        self.mention_query = None;
        self.mention_results.clear();
        self.selected_mentions.clear();
        self.error = None;
        match self.status {
            AssistantTabStatus::Running => self.queue_running_message(message),
            AssistantTabStatus::Cancelling => {
                // 消息保留为排队状态，当前回答停止后由下一次发送或自动续答消费。
            }
            AssistantTabStatus::Idle => self.start_queued_turn(cx),
        }
    }

    /// 从当前来源注册表重新固化全部根的会话快照。
    fn refresh_scope_from_app(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let Some(app_entity) = self.app.upgrade() else {
            return Err("应用已关闭，无法准备日志范围".to_string());
        };
        let app = app_entity.read(cx);
        let mut config = app.config.ai.clone();
        config.normalize();
        if app.source_registry.root_ids().is_empty() {
            return Err("尚未加载日志，请先添加包含日志文件的来源".to_string());
        }
        let Some(workspace_root) = app.source_workspace_root.clone() else {
            return Err("日志工作目录已失效，请重新加载日志".to_string());
        };
        let (registry, preferred_encoding) =
            (app.source_registry.clone(), app.selected_encoding.clone());
        let scope =
            prepare_agent_source_scope(&registry, config, &workspace_root, &preferred_encoding)
                .map_err(|error| format!("固化工作区清单失败：{error}"))?;
        // 范围准备只读取内存中的来源树，不产生模型请求；保持静默以免普通对话被误认为已开始分析。
        self.mention_candidates = Arc::new(build_mention_candidates(&scope));
        self.scope = Some(Arc::new(scope));
        self.refresh_mention_picker();
        Ok(())
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

    /// 把当前所有排队消息作为一轮初始用户输入，并启动独立可取消的模型工具循环。
    fn start_queued_turn(&mut self, cx: &mut Context<Self>) {
        if self.status != AssistantTabStatus::Idle {
            return;
        }
        let queued_messages = self
            .messages
            .iter()
            .filter_map(|message| match message {
                AssistantTabMessage::User(message)
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
        let Some(app_entity) = self.app.upgrade() else {
            self.error = Some("应用已关闭，无法继续会话".to_string());
            return;
        };
        let (mut config, config_root) = {
            let app = app_entity.read(cx);
            (
                app.config.ai.clone(),
                app.config_manager
                    .settings_path()
                    .parent()
                    .map(std::path::Path::to_path_buf)
                    .unwrap_or_else(|| std::path::PathBuf::from(".")),
            )
        };
        config.normalize();
        if let Err(error) = config.validate() {
            self.error = Some(error);
            return;
        }
        let Some(selected_model_id) = self.selected_model_id.clone() else {
            self.error = Some("当前模型配置已经失效".to_string());
            return;
        };
        let model = match config.enabled_model(&selected_model_id) {
            Ok(model) => model.clone(),
            Err(error) => {
                self.error = Some(error);
                return;
            }
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
            if let AssistantTabMessage::User(message) = message
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
        let (bash_decision_sender, bash_decision_receiver) = async_channel::bounded(8);
        let (event_sender, event_receiver) = async_channel::bounded(256);
        self.turn_generation = self.turn_generation.wrapping_add(1);
        let generation = self.turn_generation;
        self.turn_cancellation = Some(cancellation.clone());
        self.user_message_sender = Some(user_message_sender);
        self.bash_decision_sender = Some(bash_decision_sender);
        self.pending_user_messages = pending_user_messages.clone();
        self.status = AssistantTabStatus::Running;
        self.active_turn_message_start = Some(self.messages.len());
        self.active_turn_user_message_ids = active_message_ids;
        self.error = None;
        self.budget = AgentBudgetSnapshot::default();
        self.poll_turn_events(generation, event_receiver, cx);
        agent_runtime().spawn(run_assistant_turn(AssistantRunRequest {
            initial_user_messages: initial_messages,
            history: self.history.clone(),
            config,
            config_root,
            model,
            scope,
            api_key,
            cancellation,
            user_message_receiver,
            event_sender,
            pending_user_messages,
            bash_decision_receiver,
        }));
    }

    /// 批量接收一轮后台事件，防止每个 Token 单独触发界面重绘。
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
                AgentSessionStatus::Investigating => self.status = AssistantTabStatus::Running,
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
                self.pending_runtime_messages.remove(&message_id);
                self.push_trace(AgentTraceEntry::new(
                    AgentTraceKind::Warning,
                    "补充消息未进入当前回答",
                    reason,
                ));
            }
            AgentEvent::AssistantCompleted {
                output,
                accepted_user_messages,
                history_was_trimmed,
            } => {
                self.finish_answer(output.clone());
                self.history.push(AssistantHistoryTurn {
                    user_messages: accepted_user_messages,
                    assistant_output: output,
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
            AgentEvent::BashApprovalRequired {
                request_id,
                command,
            } => {
                self.push_message(AssistantTabMessage::BashApproval {
                    request_id,
                    command,
                    status: BashApprovalStatus::Pending,
                });
            }
            AgentEvent::BashApprovalOutcome {
                request_id,
                approved,
                reason,
            } => {
                let messages = Arc::make_mut(&mut self.messages);
                let index = messages.iter().rposition(|message| {
                    matches!(
                        message,
                        AssistantTabMessage::BashApproval {
                            request_id: id,
                            status: BashApprovalStatus::Pending,
                            ..
                        } if id.as_str() == request_id.as_str()
                    )
                });
                if let Some(index) = index
                    && let AssistantTabMessage::BashApproval { status, .. } = &mut messages[index]
                {
                    *status = if approved {
                        BashApprovalStatus::Approved
                    } else {
                        BashApprovalStatus::Denied(bash_approval_reason_label(&reason))
                    };
                    self.message_list.splice(index..index + 1, 1);
                }
            }
            AgentEvent::Failed(message) => {
                self.error = Some(message.clone());
                self.push_trace(AgentTraceEntry::new(
                    AgentTraceKind::Warning,
                    "助手回答失败",
                    message,
                ));
            }
        }
    }

    /// 回传用户对一条 bash 审批的答复，并即时更新卡片状态。
    fn resolve_bash_approval(&mut self, request_id: &str, approved: bool) {
        let messages = Arc::make_mut(&mut self.messages);
        let index = messages.iter().rposition(|message| {
            matches!(
                message,
                AssistantTabMessage::BashApproval {
                    request_id: id,
                    status: BashApprovalStatus::Pending,
                    ..
                } if id.as_str() == request_id
            )
        });
        let Some(index) = index else {
            return;
        };
        if let AssistantTabMessage::BashApproval { status, .. } = &mut messages[index] {
            *status = if approved {
                BashApprovalStatus::Approved
            } else {
                BashApprovalStatus::Denied("你未批准这条命令".to_string())
            };
        }
        self.message_list.splice(index..index + 1, 1);
        if let Some(sender) = &self.bash_decision_sender {
            let _ = sender.try_send(BashApprovalDecision {
                request_id: request_id.to_string(),
                approved,
            });
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
            (AgentStreamKind::Reasoning, Some(AssistantTabMessage::Reasoning(content))) => {
                content.push_str(&delta);
                self.message_list.splice(last_index..last_index + 1, 1);
            }
            (AgentStreamKind::Output, Some(AssistantTabMessage::Answer { content, .. })) => {
                content.push_str(&delta);
                self.message_list.splice(last_index..last_index + 1, 1);
            }
            (AgentStreamKind::Reasoning, _) => {
                self.push_message(AssistantTabMessage::Reasoning(delta));
            }
            (AgentStreamKind::Output, _) => {
                self.push_message(AssistantTabMessage::Answer { content: delta });
            }
        }
    }

    /// 使用最终响应校正最后一条流式正文。
    fn finish_answer(&mut self, output: String) {
        let messages = Arc::make_mut(&mut self.messages);
        let turn_start = self
            .active_turn_message_start
            .unwrap_or(messages.len())
            .min(messages.len());
        let answer_index = (turn_start..messages.len())
            .rev()
            .find(|index| matches!(messages[*index], AssistantTabMessage::Answer { .. }));
        if let Some(index) = answer_index
            && let AssistantTabMessage::Answer { content } = &mut messages[index]
        {
            if !output.trim().is_empty() {
                *content = output;
            }
            self.message_list.splice(index..index + 1, 1);
        } else {
            self.push_message(AssistantTabMessage::Answer { content: output });
        }
    }

    /// 工具轨迹连续到达时合并为一组，其余轨迹保持独立消息。
    fn push_trace(&mut self, trace: AgentTraceEntry) {
        if trace.kind == AgentTraceKind::Tool {
            let messages = Arc::make_mut(&mut self.messages);
            if let Some(AssistantTabMessage::ToolGroup { traces, .. }) = messages.last_mut() {
                traces.push(trace);
                let index = messages.len() - 1;
                self.message_list.splice(index..index + 1, 1);
                return;
            }
            self.push_message(AssistantTabMessage::ToolGroup {
                traces: vec![trace],
                is_expanded: false,
            });
        } else {
            self.push_message(AssistantTabMessage::Trace(trace));
        }
    }

    /// 追加消息并只让虚拟列表测量新增行。
    fn push_message(&mut self, message: AssistantTabMessage) {
        let messages = Arc::make_mut(&mut self.messages);
        let old_len = messages.len();
        messages.push(message);
        self.message_list.splice(old_len..old_len, 1);
    }

    /// 替换整段会话消息，用于测试直接构造消息边界场景。
    #[cfg(test)]
    fn replace_messages(&mut self, messages: Vec<AssistantTabMessage>) {
        self.messages = Arc::new(messages);
        self.message_list.reset(self.messages.len());
        self.is_following_latest = true;
    }

    /// 更新指定用户消息状态并失效对应虚拟行。
    fn update_user_message_status(&mut self, message_id: &str, status: AgentUserMessageStatus) {
        let messages = Arc::make_mut(&mut self.messages);
        if let Some((index, AssistantTabMessage::User(message))) = messages
            .iter_mut()
            .enumerate()
            .find(|(_, item)| {
                matches!(item, AssistantTabMessage::User(message) if message.message_id == message_id)
            })
        {
            message.status = status;
            self.message_list.splice(index..index + 1, 1);
        }
    }

    /// 返回是否还存在未被当前回答消费的用户消息。
    fn has_queued_messages(&self) -> bool {
        self.messages.iter().any(|message| {
            matches!(message, AssistantTabMessage::User(message) if message.status == AgentUserMessageStatus::Queued)
        })
    }

    /// 停止当前模型回答；对话历史和页面实体继续保留。
    fn stop_current_work(&mut self) {
        if !self.status.is_busy() || self.status == AssistantTabStatus::Cancelling {
            return;
        }
        self.status = AssistantTabStatus::Cancelling;
        if let Some(cancellation) = self.turn_cancellation.as_ref() {
            cancellation.cancel();
        }
    }

    /// 清理当前回答通道，让后续发送创建新的模型工具循环。
    fn finish_active_turn(&mut self) {
        for message_id in self.active_turn_user_message_ids.drain(..) {
            self.pending_runtime_messages.remove(&message_id);
        }
        self.status = AssistantTabStatus::Idle;
        self.user_message_sender = None;
        self.bash_decision_sender = None;
        self.turn_cancellation = None;
        self.pending_user_messages = Arc::new(AtomicUsize::new(0));
        self.active_turn_message_start = None;
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
                    AssistantTabMessage::Reasoning(_) | AssistantTabMessage::Answer { .. }
                )
            })
            .collect::<Vec<_>>();
        messages.extend(retained);
        self.message_list.reset(messages.len());
        if self.is_following_latest {
            self.jump_to_latest();
        }
    }

    /// 取消当前回答的 generation；会话消息与历史继续保留。
    fn cancel_background_work(&mut self) {
        if let Some(cancellation) = self.turn_cancellation.take() {
            cancellation.cancel();
        }
        self.turn_generation = self.turn_generation.wrapping_add(1);
        for message_id in self.active_turn_user_message_ids.drain(..) {
            self.pending_runtime_messages.remove(&message_id);
        }
        self.active_turn_message_start = None;
        self.user_message_sender = None;
        self.bash_decision_sender = None;
        self.pending_user_messages = Arc::new(AtomicUsize::new(0));
    }

    /// 处理文本域键盘输入，并优先响应“@”候选导航。
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
        match handle_local_input_key(&mut self.input, &event.keystroke, true, true, cx) {
            LocalInputAction::Submit => self.submit_input(cx),
            LocalInputAction::Changed => {
                self.error = None;
                if self.mention_query.is_some() || active_mention_query(&self.input).is_some() {
                    self.ensure_scope_for_mention(cx);
                }
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

    /// 首次输入“@”时按需准备来源快照，保证候选无需先发送消息。
    fn ensure_scope_for_mention(&mut self, cx: &mut Context<Self>) {
        if self.scope.is_none() {
            let _ = self.refresh_scope_from_app(cx);
        }
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

    /// 把当前高亮候选插入输入框，并保存受信来源绑定。
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

    /// 选择指定候选；替换范围按 Unicode 字符计算。
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

    /// 从来源树菜单直接插入一个工作区文件或目录；不存在候选时构造最小绑定。
    pub(crate) fn add_workspace_mention(&mut self, workspace_path: String, is_folder: bool) {
        let kind = if is_folder {
            AssistantMentionKind::Folder
        } else {
            AssistantMentionKind::File
        };
        let matching_source_count = if is_folder {
            self.scope.as_ref().map_or(0, |scope| {
                let prefix = format!("{workspace_path}/");
                scope
                    .sources
                    .iter()
                    .filter(|source| source.workspace_path.starts_with(&prefix))
                    .count()
            })
        } else {
            1
        };
        let candidate = AssistantMentionCandidate {
            search_key: workspace_path.to_lowercase(),
            display_path: workspace_path,
            kind,
            matching_source_count,
        };
        let marker = format!("@{}", candidate.display_path);
        if !self.input.value.is_empty() && !self.input.value.ends_with(char::is_whitespace) {
            self.input.value.push(' ');
        }
        self.input.value.push_str(&marker);
        self.input.value.push(' ');
        self.input.cursor = character_count(&self.input.value);
        self.input.selection_anchor = None;
        self.input.marked_range = None;
        self.input.is_focused = true;
        if !self.selected_mentions.iter().any(|selected| {
            selected.candidate.kind == candidate.kind
                && selected.candidate.display_path == candidate.display_path
        }) {
            self.selected_mentions
                .push(AssistantSelectedMention { marker, candidate });
        }
        self.mention_query = None;
        self.mention_results.clear();
        self.error = None;
    }

    /// 返回输入框当前文本，供来源树菜单集成测试断言。
    #[cfg(test)]
    pub(crate) fn input_text_for_test(&self) -> &str {
        &self.input.value
    }

    /// 返回已选来源路径，供来源树菜单集成测试断言。
    #[cfg(test)]
    pub(crate) fn selected_mention_paths_for_test(&self) -> Vec<String> {
        self.selected_mentions
            .iter()
            .map(|selected| selected.candidate.display_path.clone())
            .collect()
    }

    /// 切换“选择复制”全文模式。
    fn toggle_text_selection_mode(&mut self, cx: &mut Context<Self>) {
        self.is_text_selection_mode = !self.is_text_selection_mode;
        if self.is_text_selection_mode {
            self.selection_text = TextInputState::from_value(self.selection_transcript());
            self.selection_text.cursor = character_count(&self.selection_text.value);
            self.selection_text.selection_anchor = None;
            self.selection_text.marked_range = None;
            self.selection_text.is_focused = true;
        } else {
            self.selection_text = TextInputState::default();
        }
        cx.notify();
    }

    /// 生成选择复制模式的纯文本全文。
    fn selection_transcript(&self) -> String {
        let mut sections = Vec::new();
        for message in self.messages.iter() {
            let text = match message {
                AssistantTabMessage::User(message) => message.content.clone(),
                AssistantTabMessage::Trace(trace) => {
                    format!("{}\n{}", trace.title, trace.detail)
                }
                AssistantTabMessage::ToolGroup { traces, .. } => traces
                    .iter()
                    .map(|trace| format!("{}\n{}", trace.title, trace.detail))
                    .collect::<Vec<_>>()
                    .join("\n\n"),
                AssistantTabMessage::BashApproval {
                    command, status, ..
                } => {
                    let status = match status {
                        BashApprovalStatus::Pending => "等待确认",
                        BashApprovalStatus::Approved => "已批准",
                        BashApprovalStatus::Denied(reason) => reason.as_str(),
                    };
                    format!("{command}\n{status}")
                }
                AssistantTabMessage::Reasoning(content) => content.clone(),
                AssistantTabMessage::Answer { content } => markdown_plain_text(content),
            };
            if !text.trim().is_empty() {
                sections.push(text);
            }
        }
        sections.join("\n\n")
    }

    /// 处理选择复制模式下的键盘操作：只允许选择、光标移动和复制，不修改文本。
    fn handle_selection_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if event.keystroke.modifiers.secondary() {
            match event.keystroke.key.to_ascii_lowercase().as_str() {
                "c" => {
                    let text = self
                        .selection_text
                        .selection_range()
                        .map(|range| {
                            slice_character_range(&self.selection_text.value, range).to_string()
                        })
                        .unwrap_or_else(|| self.selection_text.value.clone());
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                "a" => {
                    self.selection_text.selection_anchor = Some(0);
                    self.selection_text.cursor = character_count(&self.selection_text.value);
                    self.selection_text.marked_range = None;
                }
                _ => {}
            }
            return;
        }
        if matches!(
            event.keystroke.key.to_ascii_lowercase().as_str(),
            "left" | "arrowleft" | "right" | "arrowright" | "home" | "end"
        ) {
            let _ =
                handle_local_input_key(&mut self.selection_text, &event.keystroke, true, false, cx);
        }
    }

    /// 渲染只读全文选择区，支持拖选和 Cmd/Ctrl+C。
    fn render_selection_textarea(
        &mut self,
        theme: &AppTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity();
        let native_entity = entity.clone();
        let native_input = NativeInput::new(self.selection_focus.clone(), move |_, _, app_cx| {
            // 只读模式也不会修改内容；保留原生输入桥接以获得焦点和选区能力。
            native_entity.update(app_cx, |_panel, panel_cx| panel_cx.notify());
        });
        let key_entity = entity.clone();
        let click_entity = entity.clone();
        let pointer_entity = entity.clone();
        div()
            .w_full()
            .h_full()
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(ASSISTANT_CONTENT_MAX_WIDTH))
                    .h_full()
                    .min_h(px(0.0))
                    .p_3()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .px_1()
                            .pb_2()
                            .text_size(px(10.0))
                            .text_color(rgb(theme.foreground_muted))
                            .child("拖选文本后按 Cmd/Ctrl+C 复制"),
                    )
                    .child(render_textarea(
                        Textarea {
                            id: "assistant-selection-text",
                            placeholder: "",
                            value: self.selection_text.value.clone(),
                            is_disabled: false,
                            is_focused: self.selection_text.is_focused,
                            cursor_index: self.selection_text.cursor,
                            selection_range: self.selection_text.selection_range(),
                            marked_range: self.selection_text.marked_range.clone(),
                            is_pointer_selecting: self.selection_text.selection_drag.is_some(),
                            visible_lines: 10,
                            fill_height: true,
                            scroll_handle: self.selection_scroll.clone(),
                            scroll_state: self.selection_scroll_state.clone(),
                            style: TextareaStyle::Composer,
                            trailing_accessory: None,
                            trailing_accessory_position: TextareaAccessoryPosition::BottomRight,
                            trailing_accessory_always_visible: false,
                            trailing_accessory_selected: false,
                            native_input: Some(native_input),
                            footer_leading: None,
                        },
                        theme,
                        move |event, _, app_cx| {
                            app_cx.stop_propagation();
                            key_entity.update(app_cx, |panel, panel_cx| {
                                panel.handle_selection_key(event, panel_cx);
                                panel_cx.notify();
                            });
                        },
                        move |_, window, app_cx| {
                            app_cx.stop_propagation();
                            click_entity.update(app_cx, |panel, panel_cx| {
                                panel.selection_text.is_focused = true;
                                panel.selection_focus.focus(window);
                                panel_cx.notify();
                            });
                        },
                        move |event: &InputPointerEvent, _, app_cx| {
                            app_cx.stop_propagation();
                            pointer_entity.update(app_cx, |panel, panel_cx| {
                                match event.action {
                                    InputPointerAction::Begin => {
                                        panel.selection_text.begin_pointer_selection(
                                            event.character_index,
                                            event.granularity,
                                        )
                                    }
                                    InputPointerAction::Extend => panel
                                        .selection_text
                                        .update_pointer_selection(event.character_index),
                                    InputPointerAction::Finish => {
                                        panel.selection_text.finish_pointer_selection()
                                    }
                                }
                                panel_cx.notify();
                            });
                        },
                        |_, _, _| {},
                    )),
            )
            .into_any_element()
    }

    /// 返回输入框焦点句柄，供菜单动作在插入来源后恢复键盘焦点。
    pub(crate) fn composer_focus_handle(&self) -> FocusHandle {
        self.input_focus.clone()
    }

    /// 展开或收起指定工具轨迹组。
    fn toggle_tool_group(&mut self, index: usize) {
        let messages = Arc::make_mut(&mut self.messages);
        if let Some(AssistantTabMessage::ToolGroup { is_expanded, .. }) = messages.get_mut(index) {
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

/// 从来源快照生成目录和文件候选；目录只保存后代数量，避免复制全部文件引用。
fn build_mention_candidates(scope: &SourceScopeSnapshot) -> Vec<AssistantMentionCandidate> {
    let mut folder_counts = BTreeMap::<String, usize>::new();
    let mut files = Vec::with_capacity(scope.sources.len());
    for source in scope.sources.iter() {
        let components = source.workspace_path.split('/').collect::<Vec<_>>();
        for component_count in 1..components.len() {
            let folder_path = components[..component_count].join("/");
            *folder_counts.entry(folder_path).or_default() += 1;
        }
        files.push(AssistantMentionCandidate {
            display_path: source.workspace_path.clone(),
            search_key: source.workspace_path.to_lowercase(),
            kind: AssistantMentionKind::File,
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
                matching_source_count,
            },
        )
        .collect::<Vec<_>>();
    candidates.extend(files);
    candidates
}

/// 解析光标所在的不含空白 token；仅以 token 起始的 `@` 触发，避免邮箱文本误弹候选。
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

/// 在模型实际消息末尾附加来源元数据；界面仍只展示用户输入的原始 @ 标记。
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
                "workspace_path": candidate.display_path,
                "path_prefix": (candidate.kind == AssistantMentionKind::Folder)
                    .then_some(candidate.display_path.as_str()),
                "matching_source_count": candidate.matching_source_count,
            })
        })
        .collect::<Vec<_>>();
    if metadata.is_empty() {
        return content.to_string();
    }
    let serialized = serde_json::to_string(&metadata).unwrap_or_else(|_| "[]".to_string());
    format!(
        "{content}\n\n<ARGUS_SELECTED_SOURCES app_generated=\"true\" note=\"Workspace-relative paths the user asked you to focus on; use them with read_file, bash or as a list_loaded_sources path_prefix.\">\n{serialized}\n</ARGUS_SELECTED_SOURCES>"
    )
}

impl Drop for AssistantTab {
    /// 页签关闭时停止当前模型和日志工具任务。
    fn drop(&mut self) {
        self.cancel_background_work();
    }
}

impl Render for AssistantTab {
    /// 渲染会话头部、虚拟消息流、状态信息和底部输入框。
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(settings_app) = self.app.upgrade() else {
            return div().size_full().into_any_element();
        };
        let entity = cx.entity();
        if !self.has_registered_scroll_handler {
            // 滚动监听闭包存放在页面自有的 ListState 内，必须持有弱引用；
            // 否则形成强引用环，页签关闭销毁页面实体时永远无法释放并执行 Drop 取消后台任务。
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
                if panel.mention_query.is_some() || active_mention_query(&panel.input).is_some() {
                    panel.ensure_scope_for_mention(panel_cx);
                }
                panel.refresh_mention_picker();
                panel_cx.notify();
            });
        });
        let unavailable_reason = self.unavailable_reason();
        let is_busy = self.status.is_busy();
        let can_stop = is_busy && self.status != AssistantTabStatus::Cancelling;
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
        let selection_area = self
            .is_text_selection_mode
            .then(|| self.render_selection_textarea(&theme, cx));
        let messages = self.messages.clone();
        let render_messages = messages.clone();
        let render_theme = theme.clone();
        let render_entity = entity.clone();
        let is_active = self.status == AssistantTabStatus::Running;
        let last_index = messages.len().saturating_sub(1);
        let model_entity = entity.clone();
        let can_switch_model = !self.status.is_busy() && self.models.len() > 1;
        let model_selector = div()
            .id("assistant-model-selector")
            .debug_selector(|| "assistant-model-selector".to_string())
            .max_w(px(280.0))
            .px_2()
            .py_1()
            .rounded_md()
            .flex()
            .items_center()
            .gap_1()
            .text_size(px(11.0))
            .text_color(rgb(theme.foreground_muted))
            .cursor_pointer()
            .when(can_switch_model, |this| {
                this.hover(|hover| hover.bg(rgb(theme.current_line)))
                    .on_click(move |_, _, app_cx| {
                        model_entity.update(app_cx, |panel, panel_cx| {
                            panel.select_next_model();
                            panel_cx.notify();
                        });
                    })
            })
            .child(div().truncate().child(model_name))
            .when(self.models.len() > 1, |this| {
                this.child(render_icon(
                    ArgusIcon::Collapse,
                    theme.foreground_muted,
                    12.0,
                ))
            })
            .into_any_element();
        let key_entity = entity.clone();
        let click_entity = entity.clone();
        let pointer_entity = entity.clone();
        let composer_action_entity = entity.clone();
        let jump_entity = entity.clone();
        let mention_entity = entity.clone();
        let blur_entity = entity.clone();
        let show_jump = !self.is_following_latest;

        div()
            .id("assistant-tab-root")
            .debug_selector(|| "assistant-tab-root".to_string())
            .size_full()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .overflow_hidden()
            // 直接落在内容面板底色上，不再使用更深的侧栏色块。
            .bg(rgb(theme.content))
            .font_family(ARGUS_UI_FONT_FAMILY)
            // 点击输入框以外的面板区域时输入框失焦：高亮边框与光标同步消失。
            .on_click(move |_, window, app_cx| {
                blur_entity.update(app_cx, |panel, panel_cx| {
                    if panel.input.is_focused {
                        panel.input.is_focused = false;
                        panel.root_focus.focus(window);
                        panel_cx.notify();
                    }
                });
            })
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
                                    .child("智能助手"),
                            )
                            .child(
                                div()
                                    .text_size(px(10.0))
                                    .text_color(rgb(theme.foreground_muted))
                                    .child(self.status.label()),
                            ),
                    )
                    .child(render_icon_button(
                        "assistant-selection-copy-toggle",
                        ArgusIcon::Copy,
                        if self.is_text_selection_mode {
                            "返回对话"
                        } else {
                            "选择复制文本"
                        },
                        self.is_text_selection_mode,
                        IconButtonSize::Small,
                        &theme,
                        cx.listener(|panel, _, window, cx| {
                            panel.toggle_text_selection_mode(cx);
                            if panel.is_text_selection_mode {
                                let focus = panel.selection_focus.clone();
                                window.on_next_frame(move |window, _| focus.focus(window));
                            }
                            cx.notify();
                        }),
                    ))
                    .child(div().flex_none().child(render_assistant_usage(
                        self.budget,
                        context_window,
                        &theme,
                    ))),
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
                // 外层铺满内容区：滚动条贴住面板右缘，与日志正文滚动条对齐。
                div()
                    .id("assistant-message-area")
                    .relative()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_hidden()
                    .on_hover(scroll_area_hover_handler("assistant".to_string()))
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .max_w(px(ASSISTANT_CONTENT_MAX_WIDTH))
                            .mx_auto()
                            .h_full()
                            .min_h(px(0.0))
                            .child(
                                list(self.message_list.clone(), move |index, _, _| {
                                    render_assistant_message(
                                        render_messages.get(index),
                                        index,
                                        is_active && index == last_index,
                                        render_entity.clone(),
                                        &render_theme,
                                    )
                                })
                                .size_full(),
                            )
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
                                                        jump_entity.update(
                                                            app_cx,
                                                            |panel, panel_cx| {
                                                                panel.jump_to_latest();
                                                                panel_cx.notify();
                                                            },
                                                        );
                                                    },
                                                )),
                                        ),
                                )
                            }),
                    )
                    .child(render_assistant_scrollbar(
                        self.message_list.clone(),
                        entity.clone(),
                        &theme,
                    ))
                    .when_some(selection_area, |this, area| {
                        this.child(
                            div()
                                .absolute()
                                .left_0()
                                .top_0()
                                .right_0()
                                .bottom_0()
                                .bg(rgb(theme.content))
                                .occlude()
                                .child(area),
                        )
                    }),
            )
            .child(
                div()
                    .debug_selector(|| "assistant-composer".to_string())
                    .flex_none()
                    .w_full()
                    .max_w(px(ASSISTANT_CONTENT_MAX_WIDTH))
                    .mx_auto()
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
                                    placeholder: "随便问点什么，Enter 发送，Shift+Enter 换行",
                                    value: self.input.value.clone(),
                                    is_disabled: self.status == AssistantTabStatus::Cancelling
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
                                    footer_leading: Some(model_selector),
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
                                        if panel.mention_query.is_some()
                                            || active_mention_query(&panel.input).is_some()
                                        {
                                            panel.ensure_scope_for_mention(panel_cx);
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
                    .when_some(self.error.clone(), |this, error| {
                        this.child(
                            div()
                                .mt_1()
                                .px_1()
                                .text_size(px(10.0))
                                .text_color(rgb(theme.error))
                                .child(error),
                        )
                    }),
            )
            .into_any_element()
    }
}

/// 渲染输入框上方的“@”来源候选，按路径深度缩进展示目录与文件。
fn render_assistant_mention_picker(
    candidates: Arc<Vec<AssistantMentionCandidate>>,
    result_indices: Vec<usize>,
    highlighted_index: usize,
    panel: Entity<AssistantTab>,
    theme: &AppTheme,
) -> AnyElement {
    let rows = result_indices
        .into_iter()
        .enumerate()
        .filter_map(|(row_index, candidate_index)| {
            let candidate = candidates.get(candidate_index)?.clone();
            let row_panel = panel.clone();
            let depth = candidate.display_path.matches('/').count();
            Some(
                div()
                    .id(("assistant-mention-candidate", candidate_index))
                    .h(px(32.0))
                    .pl(px(8.0 + depth as f32 * 12.0))
                    .pr_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .when(row_index == highlighted_index, |this| {
                        this.bg(rgb(theme.current_line))
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
        .debug_selector(|| "assistant-mention-picker".to_string())
        .mb_2()
        .max_h(px(320.0))
        .rounded_lg()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.content))
        .overflow_y_scroll()
        .child(
            div()
                .h(px(28.0))
                .px_2()
                .flex()
                .items_center()
                .text_size(px(9.0))
                .text_color(rgb(theme.foreground_muted))
                .child("选择工作区中的目录或文件"),
        )
        .when(rows.is_empty(), |this| {
            this.child(
                div()
                    .px_2()
                    .pb_2()
                    .text_size(px(10.0))
                    .text_color(rgb(theme.foreground_muted))
                    .child(if candidates.is_empty() {
                        "尚未加载日志来源"
                    } else {
                        "没有匹配的目录或文件"
                    }),
            )
        })
        .children(rows)
        .into_any_element()
}

/// 渲染一条虚拟消息；屏幕外消息不会进入当前布局树。
fn render_assistant_message(
    message: Option<&AssistantTabMessage>,
    index: usize,
    is_active: bool,
    panel: Entity<AssistantTab>,
    theme: &AppTheme,
) -> AnyElement {
    let Some(message) = message else {
        return div().into_any_element();
    };
    match message {
        AssistantTabMessage::User(message) => {
            render_user_message(message, theme).into_any_element()
        }
        AssistantTabMessage::Trace(trace) => {
            render_trace_message(trace, index, is_active, theme).into_any_element()
        }
        AssistantTabMessage::ToolGroup {
            traces,
            is_expanded,
        } => render_tool_group(traces, *is_expanded, index, is_active, panel, theme)
            .into_any_element(),
        AssistantTabMessage::BashApproval {
            request_id,
            command,
            status,
        } => {
            let decision_panel = panel.clone();
            let decision_request_id = request_id.clone();
            render_bash_approval_card(
                request_id,
                command,
                status,
                theme,
                640.0,
                move |approved, _, _, app_cx| {
                    decision_panel.update(app_cx, |panel, panel_cx| {
                        panel.resolve_bash_approval(&decision_request_id, approved);
                        panel_cx.notify();
                    });
                },
            )
            .into_any_element()
        }
        AssistantTabMessage::Reasoning(content) => {
            render_model_message("思考过程", content, true, is_active, index, theme)
                .into_any_element()
        }
        AssistantTabMessage::Answer { content } => {
            render_model_message("AI 回答", content, false, is_active, index, theme)
                .into_any_element()
        }
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

/// 渲染思考或最终回答。
fn render_model_message(
    title: &'static str,
    content: &str,
    is_reasoning: bool,
    is_active: bool,
    index: usize,
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
                ))),
        )
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
    panel: Entity<AssistantTab>,
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
        AgentTraceKind::Warning | AgentTraceKind::Status => ArgusIcon::Info,
        AgentTraceKind::Tool => ArgusIcon::Settings,
    }
}

/// 渲染标题栏右侧的 Token 与上下文占用信息。
fn render_assistant_usage(
    budget: AgentBudgetSnapshot,
    context_window: u64,
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
        .debug_selector(|| "assistant-token-usage".to_string())
        .px_1()
        .flex()
        .items_center()
        .gap_2()
        .text_size(px(9.0))
        .text_color(rgb(theme.foreground_muted))
        .child(format!("Token {} · {}", compact_tokens(total), context))
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
    panel: Entity<AssistantTab>,
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
    let thumb = div()
        .id("assistant-message-scrollbar")
        .debug_selector(|| "assistant-message-scrollbar".to_string())
        .absolute()
        .top(metrics.thumb_start)
        // 贴住内容面板右缘，与日志正文滚动条保持同一位置。
        .right(px(0.0))
        .w(px(ASSISTANT_SCROLLBAR_WIDTH))
        .h(metrics.thumb_length)
        .rounded_lg()
        .bg(rgb(theme.foreground_muted))
        .cursor_pointer()
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
                            // 捕获阶段收尾拖拽：冒泡路径上的停传播（如输入框拖选、其他浮层）
                            // 可能吞掉抬起事件；一旦漏收，拖拽起始高度会永久残留，
                            // 内容未溢出时也会画出幻影滚动条。
                            if !phase.capture() || event.button != MouseButton::Left {
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
        );
    // 动画包在最外层，滚动停止 2 秒后自动淡出。
    with_scrollbar_visibility(thumb, "assistant", 0.5).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigManager;
    use crate::config::paths::isolated_test_dir;
    use gpui::{AppContext, TestAppContext};

    /// 验证新建智能助手页处于空闲态，不自动启动任何分析或模型会话。
    #[gpui::test]
    fn assistant_tab_starts_idle_without_auto_analysis(cx: &mut TestAppContext) {
        let directory = isolated_test_dir("assistant-tab-idle");
        let manager = ConfigManager::new(directory.join("settings.toml"));
        let app = cx.new(|_| ArgusApp::new_with_config_manager(manager));
        let panel = cx.new(|panel_cx| {
            AssistantTab::new(app.clone(), AppTheme::dark(), AiConfig::default(), panel_cx)
        });

        panel.update(cx, |panel, _panel_cx| {
            assert_eq!(panel.status, AssistantTabStatus::Idle);
            assert!(panel.history.is_empty());
            assert!(panel.user_message_sender.is_none());
            assert!(panel.turn_cancellation.is_none());
            assert_eq!(panel.messages.len(), 1, "新页面只应包含一条欢迎说明");
        });
    }

    /// 验证“@”查询使用 Unicode 字符范围，邮箱式正文不会误触发候选。
    #[test]
    fn mention_query_only_matches_current_at_token() {
        let input = TextInputState::from_value("检查一下 @memory".to_string());
        let query = active_mention_query(&input).expect("独立 @ token 应触发候选");
        assert_eq!(query.text, "memory");
        assert_eq!(query.range, 5..12);

        let email = TextInputState::from_value("联系 user@example.com".to_string());
        assert!(active_mention_query(&email).is_none());
    }

    /// 验证选择候选后插入可见标记，并只在模型消息中附带来源元数据。
    #[gpui::test]
    fn mention_selection_inserts_marker_and_runtime_metadata(cx: &mut TestAppContext) {
        let directory = isolated_test_dir("assistant-mention-select");
        let manager = ConfigManager::new(directory.join("settings.toml"));
        let app = cx.new(|_| ArgusApp::new_with_config_manager(manager));
        let panel = cx.new(|panel_cx| {
            AssistantTab::new(app.clone(), AppTheme::dark(), AiConfig::default(), panel_cx)
        });

        panel.update(cx, |panel, _| {
            panel.mention_candidates = Arc::new(vec![AssistantMentionCandidate {
                display_path: "logs/app.log".to_string(),
                search_key: "logs/app.log".to_string(),
                kind: AssistantMentionKind::File,
                matching_source_count: 1,
            }]);
            panel.input = TextInputState::from_value("@app".to_string());
            panel.input.cursor = 4;
            panel.refresh_mention_picker();
            assert_eq!(panel.mention_results, vec![0]);
            assert!(panel.select_highlighted_mention());
            assert_eq!(panel.input.value, "@logs/app.log ");
            assert_eq!(panel.selected_mentions.len(), 1);

            let runtime =
                assistant_message_with_mentions("检查 @logs/app.log", &panel.selected_mentions);
            assert!(runtime.contains("ARGUS_SELECTED_SOURCES"));
            assert!(runtime.contains("logs/app.log"));
            assert_eq!(
                assistant_message_with_mentions("检查全部日志", &panel.selected_mentions),
                "检查全部日志"
            );
        });
    }

    /// 验证来源树菜单注入的路径会追加到输入框并登记为已选来源。
    #[gpui::test]
    fn add_workspace_mention_appends_visible_marker(cx: &mut TestAppContext) {
        let directory = isolated_test_dir("assistant-mention-menu");
        let manager = ConfigManager::new(directory.join("settings.toml"));
        let app = cx.new(|_| ArgusApp::new_with_config_manager(manager));
        let panel = cx.new(|panel_cx| {
            AssistantTab::new(app.clone(), AppTheme::dark(), AiConfig::default(), panel_cx)
        });

        panel.update(cx, |panel, _| {
            panel.input = TextInputState::from_value("看看".to_string());
            panel.input.cursor = 2;
            panel.add_workspace_mention("logs/app.log".to_string(), false);
            assert_eq!(panel.input.value, "看看 @logs/app.log ");
            assert_eq!(
                panel.selected_mention_paths_for_test(),
                vec!["logs/app.log"]
            );
        });
    }

    /// 验证没有加载日志时发送只报错，不创建用户消息或后台会话。
    #[gpui::test]
    fn assistant_tab_submit_without_sources_does_not_start_turn(cx: &mut TestAppContext) {
        let directory = isolated_test_dir("assistant-tab-no-sources");
        let manager = ConfigManager::new(directory.join("settings.toml"));
        let app = cx.new(|_| ArgusApp::new_with_config_manager(manager));
        let panel = cx.new(|panel_cx| {
            AssistantTab::new(app.clone(), AppTheme::dark(), AiConfig::default(), panel_cx)
        });

        panel.update(cx, |panel, panel_cx| {
            let mut profile = AiModelProfile::new();
            profile.name = "测试模型".to_string();
            profile.base_url = "http://localhost/v1".to_string();
            profile.model = "test-model".to_string();
            panel.selected_model_id = Some(profile.profile_id.clone());
            panel.models = vec![profile];
            panel.input = TextInputState::from_value("检查当前日志".to_string());

            panel.submit_input(panel_cx);

            assert_eq!(panel.status, AssistantTabStatus::Idle);
            assert!(panel.history.is_empty());
            assert!(panel.user_message_sender.is_none());
            assert_eq!(panel.messages.len(), 1, "发送失败不应写入用户消息");
            assert!(
                panel
                    .error
                    .as_deref()
                    .unwrap_or_default()
                    .contains("尚未加载日志"),
                "应提示先加载日志：{:?}",
                panel.error
            );
        });
    }

    /// 验证一轮完成后写入中立历史并回到空闲态，下一轮可继续发送。
    #[gpui::test]
    fn assistant_tab_completed_turn_keeps_history_and_returns_idle(cx: &mut TestAppContext) {
        let directory = isolated_test_dir("assistant-tab-history");
        let manager = ConfigManager::new(directory.join("settings.toml"));
        let app = cx.new(|_| ArgusApp::new_with_config_manager(manager));
        let panel = cx.new(|panel_cx| {
            AssistantTab::new(app.clone(), AppTheme::dark(), AiConfig::default(), panel_cx)
        });

        panel.update(cx, |panel, panel_cx| {
            panel.status = AssistantTabStatus::Running;
            panel.active_turn_message_start = Some(panel.messages.len());
            panel.apply_turn_event(
                AgentEvent::AssistantCompleted {
                    output: "这是回答".to_string(),
                    accepted_user_messages: vec!["这是问题".to_string()],
                    history_was_trimmed: false,
                },
                0,
                panel_cx,
            );

            assert_eq!(panel.status, AssistantTabStatus::Idle);
            assert_eq!(panel.history.len(), 1);
            assert_eq!(panel.history[0].user_messages, vec!["这是问题"]);
            assert_eq!(panel.history[0].assistant_output, "这是回答");
            assert!(panel.messages.iter().any(|message| matches!(
                message,
                AssistantTabMessage::Answer { content } if content == "这是回答"
            )));
        });
    }

    /// 验证进入选择复制模式后，全文输入状态被填充并处于聚焦态。
    #[gpui::test]
    fn text_selection_mode_populates_readonly_transcript(cx: &mut TestAppContext) {
        let directory = isolated_test_dir("assistant-selection-mode");
        let manager = ConfigManager::new(directory.join("settings.toml"));
        let app = cx.new(|_| ArgusApp::new_with_config_manager(manager));
        let panel = cx.new(|panel_cx| {
            AssistantTab::new(app.clone(), AppTheme::dark(), AiConfig::default(), panel_cx)
        });

        panel.update(cx, |panel, panel_cx| {
            panel.replace_messages(vec![AssistantTabMessage::User(AgentUserMessage::queued(
                "你好".to_string(),
            ))]);
            panel.toggle_text_selection_mode(panel_cx);
            assert!(panel.is_text_selection_mode);
            assert_eq!(panel.selection_text.value, "你好");
            assert!(panel.selection_text.is_focused);
        });
    }

    /// 验证选择复制模式生成的全文包含用户消息、回答和工具轨迹。
    #[gpui::test]
    fn selection_transcript_contains_visible_message_text(cx: &mut TestAppContext) {
        let directory = isolated_test_dir("assistant-selection-transcript");
        let manager = ConfigManager::new(directory.join("settings.toml"));
        let app = cx.new(|_| ArgusApp::new_with_config_manager(manager));
        let panel = cx.new(|panel_cx| {
            AssistantTab::new(app.clone(), AppTheme::dark(), AiConfig::default(), panel_cx)
        });

        panel.update(cx, |panel, _| {
            panel.replace_messages(vec![
                AssistantTabMessage::User(AgentUserMessage::queued("请分析卡顿".to_string())),
                AssistantTabMessage::Trace(AgentTraceEntry::new(
                    AgentTraceKind::Warning,
                    "提示",
                    "工具输出已截断",
                )),
                AssistantTabMessage::Answer {
                    content: "**结论**：连接池耗尽".to_string(),
                },
            ]);
            let transcript = panel.selection_transcript();
            assert!(transcript.contains("请分析卡顿"));
            assert!(transcript.contains("工具输出已截断"));
            assert!(transcript.contains("结论"));
            assert!(transcript.contains("连接池耗尽"));
        });
    }

    /// 验证当前轮没有流式正文时，最终回答会新增消息而不会覆盖上一轮答案。
    #[gpui::test]
    fn final_answer_without_stream_delta_does_not_overwrite_previous_turn(cx: &mut TestAppContext) {
        let directory = isolated_test_dir("assistant-final-answer-boundary");
        let manager = ConfigManager::new(directory.join("settings.toml"));
        let app = cx.new(|_| ArgusApp::new_with_config_manager(manager));
        let panel = cx.new(|panel_cx| {
            AssistantTab::new(app.clone(), AppTheme::dark(), AiConfig::default(), panel_cx)
        });

        panel.update(cx, |panel, _panel_cx| {
            let mut current_question = AgentUserMessage::queued("当前问题".to_string());
            current_question.status = AgentUserMessageStatus::Consumed;
            panel.replace_messages(vec![
                AssistantTabMessage::Answer {
                    content: "上一轮答案".to_string(),
                },
                AssistantTabMessage::User(current_question),
            ]);
            panel.active_turn_message_start = Some(2);

            panel.finish_answer("当前轮答案".to_string());

            assert!(matches!(
                panel.messages.first(),
                Some(AssistantTabMessage::Answer { content, .. }) if content == "上一轮答案"
            ));
            assert!(matches!(
                panel.messages.get(2),
                Some(AssistantTabMessage::Answer { content, .. }) if content == "当前轮答案"
            ));
        });
    }
}
