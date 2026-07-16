//! 文件职责：连接来源树智能分析入口、问题模态框、Agent 独立窗口和后台编排任务。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-16
//! 作者：Argus 开发团队
//! 主要功能：解析分析根范围、读取系统凭据、先创建独立窗口，再在专用 Tokio 运行时启动单会话 Agent。

use gpui::{AppContext, Bounds, Context, WindowBounds, WindowOptions, px, size};

use crate::agent::{
    AgentLogProfileMatchSummary, AgentRunRequest, AgentSourcePreparation, SourceScopeSnapshot,
    agent_runtime, load_api_key, prepare_agent_source_scope, run_agent_session,
};
use crate::app::{ArgusApp, frameless_resizable_titlebar};
use crate::config::{AiConfig, AiModelProfile};
use crate::loader::SourceId;
use crate::search::search_engine::SearchResult;
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
        // 底层单次归档枚举完成后，扫描会在下一节点边界观察取消；generation 同时阻止过期结果回填。
        if let Some(cancellation) = self.ai_agent_source_scan_cancellation.take() {
            cancellation.cancel();
        }
        self.ai_agent_source_scan_generation = self.ai_agent_source_scan_generation.wrapping_add(1);
        self.ai_agent_launch_modal = None;
        self.placeholder_notice = "已取消智能分析问题输入".to_string();
    }

    /// 校验配置和范围，在后台完整扫描来源根后创建独立窗口并启动 Agent。
    ///
    /// 返回值：窗口和后台任务成功建立时返回 `Ok`；失败时保留问题模态框并展示错误。
    pub(crate) fn start_ai_agent_session(
        &mut self,
        question: String,
        model_profile_id: String,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if let Some(window_handle) = self.ai_agent_window_handle
            && window_handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        {
            return Err("已有智能分析会话正在窗口中运行".to_string());
        }
        self.ai_agent_window_handle = None;
        let mut config = self.config.ai.clone();
        config.normalize();
        config.validate()?;
        let model = config.enabled_model(&model_profile_id)?.clone();
        // 启动扫描前先验证凭据，避免长时间枚举来源后才发现模型不可用；真正启动时会再次读取最新密钥。
        load_api_key(&model.base_url)?;
        self.ai_agent_scope_unavailable_reason()
            .map_or(Ok(()), Err)?;

        let scan_generation = self.ai_agent_source_scan_generation.wrapping_add(1);
        self.ai_agent_source_scan_generation = scan_generation;
        if let Some(previous) = self.ai_agent_source_scan_cancellation.take() {
            previous.cancel();
        }
        let scan_cancellation = tokio_util::sync::CancellationToken::new();
        self.ai_agent_source_scan_cancellation = Some(scan_cancellation.clone());
        let registry = self.source_registry.clone();
        let selected_id = self.source_registry.selected_id();
        let default_encoding = self.selected_encoding.clone();
        let loader_config = self.config.loader.clone();
        let archive_passwords = self.archive_passwords.clone();
        let scan_config = config.clone();
        let scan_loader_config = loader_config.clone();
        let scan_archive_passwords = archive_passwords.clone();
        self.placeholder_notice = "正在完整扫描来源树并匹配日志类型".to_string();

        cx.spawn(async move |view, cx| {
            let preparation = cx
                .background_executor()
                .spawn(async move {
                    prepare_agent_source_scope(
                        registry,
                        selected_id,
                        scan_config,
                        default_encoding,
                        scan_loader_config,
                        scan_archive_passwords,
                        scan_cancellation,
                    )
                })
                .await;
            view.update(cx, |app, cx| {
                app.finish_ai_agent_source_scan(
                    scan_generation,
                    question,
                    config,
                    model,
                    preparation,
                    cx,
                );
                cx.notify();
            })
            .ok();
        })
        .detach();
        Ok(())
    }

    /// 接收后台完整扫描结果；过期 generation 直接丢弃，禁止用户关闭后仍自动启动会话。
    fn finish_ai_agent_source_scan(
        &mut self,
        scan_generation: usize,
        question: String,
        config: AiConfig,
        model: AiModelProfile,
        preparation: Result<AgentSourcePreparation, String>,
        cx: &mut Context<Self>,
    ) {
        if scan_generation != self.ai_agent_source_scan_generation
            || self.ai_agent_launch_modal.is_none()
        {
            return;
        }
        self.ai_agent_source_scan_cancellation = None;
        let preparation = match preparation {
            Ok(preparation) => preparation,
            Err(error) => {
                self.finish_ai_agent_preparing_with_error(
                    format!("来源树完整扫描失败：{error}"),
                    cx,
                );
                return;
            }
        };

        let AgentSourcePreparation {
            registry,
            scope,
            warnings,
            match_summaries,
            source_scan_elapsed_seconds,
            profile_elapsed_seconds,
        } = preparation;
        // 回填与生成快照使用同一注册表副本，确保报告中的内部来源 ID 可以继续导航到主窗口。
        self.source_child_load_generations.clear();
        self.clear_source_archive_probe_state();
        self.source_registry = registry;
        self.rebuild_filtered_source_ids();
        if let Err(error) = self.launch_prepared_ai_agent(
            question,
            config,
            model,
            scope,
            match_summaries,
            warnings.len(),
            source_scan_elapsed_seconds,
            profile_elapsed_seconds,
            cx,
        ) {
            self.finish_ai_agent_preparing_with_error(error, cx);
        }
    }

    /// 使用已经完整扫描并匹配日志类型的来源快照创建窗口和后台模型会话。
    fn launch_prepared_ai_agent(
        &mut self,
        question: String,
        config: AiConfig,
        model: AiModelProfile,
        scope: SourceScopeSnapshot,
        match_summaries: Vec<AgentLogProfileMatchSummary>,
        warning_count: usize,
        source_scan_elapsed_seconds: u64,
        profile_elapsed_seconds: u64,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let context_window_tokens = model.context_window_tokens;
        let source_count = scope.sources.len();
        let profile_count = scope.profiles.len();
        let scope = std::sync::Arc::new(scope);
        let api_key = load_api_key(&model.base_url)?;
        let session_id = scope.session_id.clone();
        let cancellation = crate::agent::session::new_cancellation_token();
        let (user_message_sender, user_message_receiver) = async_channel::bounded(20);
        let pending_user_messages = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let user_message_gate = std::sync::Arc::new(std::sync::Mutex::new(true));
        let (event_sender, event_receiver) = async_channel::bounded(256);
        let config_root = self
            .config_manager
            .settings_path()
            .parent()
            .map(std::path::Path::to_path_buf)
            .ok_or_else(|| "无法解析 AI 报告保存目录".to_string())?;

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
                        context_window_tokens,
                        cx,
                    )
                })
            })
            .map_err(|error| format!("创建智能分析独立窗口失败：{error}"))?;

        // 只有独立窗口创建成功后才关闭问题模态框并启动后台任务，避免问题草稿丢失。
        self.ai_agent_window_handle = Some(window_handle);
        self.ai_agent_launch_modal = None;
        self.placeholder_notice = format!(
            "已扫描 {source_count} 个日志文件并匹配 {profile_count} 种日志类型，启动会话 {}{}",
            session_id.chars().take(8).collect::<String>(),
            if warning_count > 0 {
                format!("（{warning_count} 项扫描警告）")
            } else {
                String::new()
            }
        );
        agent_runtime().spawn(run_agent_session(AgentRunRequest {
            question,
            config,
            model,
            scope,
            api_key,
            config_root,
            cancellation,
            user_message_receiver,
            event_sender,
            pending_user_messages,
            user_message_gate,
            source_scan_elapsed_seconds,
            profile_elapsed_seconds,
        }));
        Ok(())
    }

    /// 把来源扫描或窗口创建错误回写到仍打开的问题对话框，允许用户原地重试。
    fn finish_ai_agent_preparing_with_error(&mut self, message: String, cx: &mut Context<Self>) {
        if let Some(dialog) = self.ai_agent_launch_modal.as_ref() {
            dialog.update(cx, |dialog, dialog_cx| {
                dialog.finish_preparing_with_error(message.clone());
                dialog_cx.notify();
            });
        }
        self.placeholder_notice = message;
    }

    /// 从 Agent 报告证据引用打开主窗口日志并定位到首行。
    ///
    /// 参数说明：
    /// - `source_id`：会话快照中解析得到的内部来源 ID；模型不能直接提供该值。
    /// - `line`：报告中的 1 基证据首行。
    /// - `cx`：用于在日志尚未加载时启动现有异步读取流程。
    pub(crate) fn open_ai_evidence(
        &mut self,
        source_id: SourceId,
        line: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(node) = self.source_registry.node(source_id).cloned() else {
            self.placeholder_notice =
                "证据来源已不在当前来源树中，请重新加载来源后复核".to_string();
            return;
        };
        if !node.kind.is_log_candidate() {
            self.placeholder_notice = "证据来源已变化，当前节点不再是可读取日志".to_string();
            return;
        }
        self.select_source(source_id);
        self.request_open_log_content(source_id, cx);
        self.scroll_source_into_view(source_id);

        // 复用现有搜索结果待定位机制：日志尚在后台读取时，读取完成回调会自动滚动并高亮目标行。
        self.log_search.pending_activation = Some(SearchResult {
            source_id,
            label: node.label.clone(),
            path: node.label,
            line_number: line.saturating_sub(1),
            line_text: String::new(),
            match_ranges: Vec::new(),
            matched_keywords: Vec::new(),
        });
        self.finish_pending_search_activation(source_id);
        self.placeholder_notice = format!("正在定位 Agent 证据第 {} 行", line.max(1));
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
        if self.source_registry.root_ids().len() > 1 && self.source_registry.selected_id().is_none()
        {
            return Some("当前存在多个日志来源，请先在来源树中选择要分析的节点".to_string());
        }
        None
    }
}
