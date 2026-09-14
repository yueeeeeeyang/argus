//! 文件职责：连接来源树智能分析入口、问题模态框、Agent 独立窗口和后台通用智能体任务。
//! 创建日期：2026-07-15
//! 修改日期：2026-09-14
//! 作者：Argus 开发团队
//! 主要功能：解析分析根范围、读取系统凭据、先创建独立窗口，再在专用 Tokio 运行时启动通用智能体循环。

use gpui::{AppContext, Bounds, Context, WindowBounds, WindowOptions, px, size};
use std::sync::Arc;
use std::time::Instant;

use crate::agent::{
    AgentLoopNote, AgentLoopRequest, AgentSourcePreparation, agent_runtime, load_api_key,
    prepare_agent_source_scope, run_agent_loop,
};
use crate::app::{ArgusApp, frameless_resizable_titlebar};
use crate::config::{AiConfig, AiModelProfile};
use crate::ui::agent_dialog::AgentLaunchDialog;
use crate::ui::agent_window::AgentWindow;

/// Agent 独立窗口默认宽度。
const AGENT_WINDOW_WIDTH: f32 = 1120.0;
/// Agent 独立窗口默认高度。
const AGENT_WINDOW_HEIGHT: f32 = 760.0;
/// Agent 独立窗口最小宽度。
const AGENT_WINDOW_MIN_WIDTH: f32 = 860.0;
/// Agent 独立窗口最小高度。
const AGENT_WINDOW_MIN_HEIGHT: f32 = 600.0;

impl ArgusApp {
    /// 打开初始问题模态框；已有 Agent 窗口仍有效时直接置前。
    pub(crate) fn open_ai_agent_launch_dialog(&mut self, cx: &mut Context<Self>) {
        if let Some(window_handle) = self.ai_agent_window_handle
            && window_handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        {
            self.placeholder_notice = "智能分析窗口已显示到最前".to_string();
            return;
        }
        self.ai_agent_window_handle = None;
        if self.ai_agent_launch_modal.is_some() {
            self.placeholder_notice = "智能分析问题输入框已经打开".to_string();
            return;
        }
        let scope_label = self.ai_agent_scope_label();
        let mut config = self.config.ai.clone();
        config.normalize();
        let config_error = config.validate().err();
        // 入口只展示能够从系统凭据库读取密钥的模型，避免用户填写问题后才发现所选模型不可用。
        let models = if config_error.is_none() {
            config
                .model_profiles
                .iter()
                .filter(|model| model.enabled && load_api_key(&model.base_url).is_ok())
                .cloned()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let credential_error = (config_error.is_none() && models.is_empty())
            .then(|| "已启用模型均未找到可用 API Key，请先在模型配置中保存密钥".to_string());
        let unavailable_reason = self
            .ai_agent_scope_unavailable_reason()
            .or(config_error)
            .or(credential_error);
        let is_available = !models.is_empty() && unavailable_reason.is_none();
        let app = cx.entity();
        let theme = self.theme.clone();
        self.ai_agent_launch_modal = Some(cx.new(|cx| {
            AgentLaunchDialog::new(
                app,
                theme,
                scope_label,
                models,
                unavailable_reason,
                config.allow_raw_log_content,
                cx,
            )
        }));
        self.clear_all_text_input_focus();
        self.placeholder_notice = if is_available {
            "请输入需要 Agent 分析的问题".to_string()
        } else {
            "当前智能分析不可用，已显示原因".to_string()
        };
    }

    /// 关闭初始问题模态框，不影响已经启动的独立 Agent 窗口。
    pub(crate) fn close_ai_agent_launch_dialog(&mut self) {
        self.ai_agent_launch_modal = None;
        self.placeholder_notice = "已取消智能分析问题输入".to_string();
    }

    /// 校验配置和范围，固化工作区清单后创建独立窗口并启动通用智能体循环。
    ///
    /// 返回值：窗口和后台任务成功建立时返回 `Ok`；失败时保留问题模态框并展示错误。
    pub(crate) fn start_ai_agent_session(
        &mut self,
        question: String,
        model_profile_id: String,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        // 旧窗口已进入终态时关闭重建；仍在运行时拒绝并发会话。
        if let Some(window_handle) = self.ai_agent_window_handle.take() {
            let is_idle = window_handle
                .update(cx, |window, _, _| window.is_session_terminal())
                .unwrap_or(true);
            if is_idle {
                let _ = window_handle.update(cx, |_, window, _| window.remove_window());
            } else {
                let _ = window_handle.update(cx, |_, window, _| window.activate_window());
                self.ai_agent_window_handle = Some(window_handle);
                return Err(
                    "已有智能分析会话正在运行，请先取消或等待完成后再发起新分析".to_string()
                );
            }
        }
        let mut config = self.config.ai.clone();
        config.normalize();
        config.validate()?;
        let model = config.enabled_model(&model_profile_id)?.clone();
        // 启动前先验证凭据，避免会话建立后才发现模型不可用；真正启动时会再次读取最新密钥。
        load_api_key(&model.base_url)?;
        self.ai_agent_scope_unavailable_reason()
            .map_or(Ok(()), Err)?;
        let workspace_root = self
            .source_workspace_root
            .clone()
            .ok_or_else(|| "日志工作目录已失效，请重新加载日志来源".to_string())?;
        // 整体耗时从用户提交问题后正式开始准备来源时计算。
        let analysis_started_at = Instant::now();
        let preparation = prepare_agent_source_scope(
            &self.source_registry,
            self.source_registry.selected_id(),
            config.clone(),
            &workspace_root,
            &self.selected_encoding,
        );

        if let Err(error) = self.launch_prepared_ai_agent(
            analysis_started_at,
            question,
            config,
            model,
            preparation,
            cx,
        ) {
            self.finish_ai_agent_preparing_with_error(error, cx);
        }
        Ok(())
    }

    /// 使用已固化的工作区清单创建窗口和后台通用智能体会话。
    fn launch_prepared_ai_agent(
        &mut self,
        analysis_started_at: Instant,
        question: String,
        config: AiConfig,
        model: AiModelProfile,
        preparation: Result<AgentSourcePreparation, String>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let preparation = preparation.map_err(|error| format!("固化分析范围失败：{error}"))?;
        let AgentSourcePreparation {
            scope,
            match_summaries,
        } = preparation;
        let context_window_tokens = model.context_window_tokens;
        let source_count = scope.sources.len();
        let profile_count = scope.profiles.len();
        let scope = Arc::new(scope);
        let api_key = load_api_key(&model.base_url)?;
        let session_id = scope.session_id.clone();
        let cancellation = crate::agent::session::new_cancellation_token();
        let (user_message_sender, user_message_receiver) = async_channel::bounded(20);
        let pending_user_messages = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let user_message_gate = Arc::new(std::sync::Mutex::new(true));
        let (bash_decision_sender, bash_decision_receiver) = async_channel::bounded(8);
        let (event_sender, event_receiver) = async_channel::bounded(256);

        let app = cx.entity();
        let initial_theme = self.theme.clone();
        let window_question = question.clone();
        let window_session_id = session_id.clone();
        let window_cancellation = cancellation.clone();
        let window_pending_user_messages = pending_user_messages.clone();
        let window_user_message_gate = user_message_gate.clone();
        let window_scope = scope.clone();
        let window_match_summaries = match_summaries;
        let bounds = Bounds::centered(
            None,
            size(px(AGENT_WINDOW_WIDTH), px(AGENT_WINDOW_HEIGHT)),
            cx,
        );
        let window_options = WindowOptions {
            titlebar: Some(frameless_resizable_titlebar()),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(
                px(AGENT_WINDOW_MIN_WIDTH),
                px(AGENT_WINDOW_MIN_HEIGHT),
            )),
            ..Default::default()
        };
        let window_handle = cx
            .open_window(window_options, move |_, cx| {
                cx.new(|cx| {
                    AgentWindow::new(
                        app,
                        initial_theme,
                        window_session_id,
                        window_question,
                        user_message_sender,
                        event_receiver,
                        window_cancellation,
                        window_pending_user_messages,
                        window_user_message_gate,
                        window_scope,
                        window_match_summaries,
                        bash_decision_sender,
                        context_window_tokens,
                        analysis_started_at,
                        cx,
                    )
                })
            })
            .map_err(|error| format!("创建智能分析独立窗口失败：{error}"))?;

        // 只有独立窗口创建成功后才关闭问题模态框并启动后台任务，避免问题草稿丢失。
        self.ai_agent_window_handle = Some(window_handle);
        self.ai_agent_launch_modal = None;
        self.placeholder_notice = format!(
            "已固化 {source_count} 个日志文件并匹配 {profile_count} 种日志类型，启动会话 {}",
            session_id.chars().take(8).collect::<String>()
        );
        agent_runtime().spawn(run_agent_loop(AgentLoopRequest {
            note: AgentLoopNote::Analysis,
            question,
            history: Vec::new(),
            history_was_trimmed: false,
            initial_user_messages: Vec::new(),
            config,
            model,
            scope,
            api_key,
            cancellation,
            user_message_receiver,
            event_sender,
            pending_user_messages,
            user_message_gate: Some(user_message_gate),
            bash_decision_receiver,
        }));
        Ok(())
    }

    /// 把范围固化或窗口创建错误回写到仍打开的问题对话框，允许用户原地重试。
    fn finish_ai_agent_preparing_with_error(&mut self, message: String, cx: &mut Context<Self>) {
        if let Some(dialog) = self.ai_agent_launch_modal.as_ref() {
            dialog.update(cx, |dialog, dialog_cx| {
                dialog.finish_preparing_with_error(message.clone());
                dialog_cx.notify();
            });
        }
        self.placeholder_notice = message;
    }

    /// 返回问题模态框预览的来源根名称。
    fn ai_agent_scope_label(&self) -> String {
        if let Some(selected_id) = self.source_registry.selected_id()
            && let Some(root_id) = self.source_registry.root_id_for(selected_id)
            && let Some(root) = self.source_registry.node(root_id)
        {
            return root.label.clone();
        }
        if self.source_registry.root_ids().len() == 1
            && let Some(root) = self
                .source_registry
                .node(self.source_registry.root_ids()[0])
        {
            return root.label.clone();
        }
        if self.source_registry.root_ids().is_empty() {
            "尚未加载来源".to_string()
        } else {
            "存在多个来源，请先选择一个来源树节点".to_string()
        }
    }

    /// 返回来源树是否允许启动新会话的用户可读原因，供点击入口时直接预检。
    fn ai_agent_scope_unavailable_reason(&self) -> Option<String> {
        if self.source_registry.root_ids().is_empty() {
            return Some("尚未加载日志来源，请先添加包含日志文件的来源".to_string());
        }
        if self.source_workspace_root.is_none() {
            return Some("日志工作目录已失效，请重新加载日志来源".to_string());
        }
        if self.source_registry.root_ids().len() > 1 && self.source_registry.selected_id().is_none()
        {
            return Some("当前存在多个日志来源，请先在来源树中选择要分析的节点".to_string());
        }
        None
    }
}
