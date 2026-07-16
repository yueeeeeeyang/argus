//! 文件职责：渲染智能分析的模型配置、日志类型说明与默认系统提示词子对话框。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-16
//! 作者：Argus 开发团队
//! 主要功能：按独立模式管理 OpenAI 兼容端点、API Key、原文授权、日志类型匹配规则、分析说明和专业提示词。

use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, FocusHandle, FontWeight, IntoElement,
    KeyDownEvent, Render, ScrollHandle, Subscription, Window, canvas, div, prelude::*, px, rgb,
};
use secrecy::SecretString;

use crate::app::{ArgusApp, TextInputState};
use crate::config::{
    AI_RAW_LOG_CONSENT_VERSION, AiConfig, AiModelProfile, DEFAULT_AI_SYSTEM_PROMPT, LogNameMatcher,
    LogNameMatcherMode, LogNameMatcherTarget, LogTypeProfile,
};
use crate::fonts::ARGUS_UI_FONT_FAMILY;
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::components::input::{
    Input, InputAccessory, InputPointerAction, InputPointerEvent, InputSize, NativeInput, Textarea,
    TextareaAccessoryPosition, TextareaScrollState, TextareaStyle, render_input, render_textarea,
};
use crate::ui::components::input_behavior::{LocalInputAction, handle_local_input_key};
use crate::ui::components::modal_dialog::{ModalDialog, render_modal_dialog};
use crate::ui::components::scrollbar::scrollbar_metrics;

/// 模型配置对话框宽度；保持在常规表单对话框范围内，避免三项输入被无意义拉宽。
const AI_MODEL_EDITOR_WIDTH: f32 = 680.0;
/// 模型配置对话框高度。
const AI_MODEL_EDITOR_HEIGHT: f32 = 520.0;
/// 单项日志类型说明对话框宽度；列表已直接展示在设置页，这里只承载新增或编辑表单。
const AI_LOG_PROFILE_EDITOR_WIDTH: f32 = 680.0;
/// 单项日志类型说明对话框高度。
const AI_LOG_PROFILE_EDITOR_HEIGHT: f32 = 600.0;
/// 默认系统提示词编辑对话框宽度，给较长的专业指令保留可读行宽。
const AI_SYSTEM_PROMPT_EDITOR_WIDTH: f32 = 760.0;
/// 默认系统提示词编辑对话框高度。
const AI_SYSTEM_PROMPT_EDITOR_HEIGHT: f32 = 620.0;
/// 编辑器标题栏统一高度。
const AI_EDITOR_HEADER_HEIGHT: f32 = 56.0;
/// 编辑器底部操作区统一高度。
const AI_EDITOR_FOOTER_HEIGHT: f32 = 60.0;
/// 编辑器滚动条宽度。
const AI_EDITOR_SCROLLBAR_WIDTH: f32 = 8.0;
/// 编辑器纵向滚动条滑块宽度。
const AI_EDITOR_SCROLLBAR_THUMB_WIDTH: f32 = 4.0;
/// 编辑器纵向滚动滑块相对可滚动区域的上下留白。
const AI_EDITOR_SCROLLBAR_PADDING: f32 = 2.0;
/// 编辑器纵向滚动条滑块最小高度。
const AI_EDITOR_SCROLLBAR_MIN_THUMB: f32 = 18.0;
/// 智能分析设置子对话框类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AiSettingsEditorKind {
    /// 新增模型，或按设置列表索引编辑现有模型。
    Model(Option<usize>),
    /// 新增日志类型，或按设置列表索引编辑现有日志类型。
    LogProfile(Option<usize>),
    /// 编辑全局默认系统提示词。
    SystemPrompt,
}

impl AiSettingsEditorKind {
    /// 返回对话框标题，供标题栏和主应用状态提示统一使用。
    pub(crate) fn dialog_title(self) -> &'static str {
        match self {
            Self::Model(Some(_)) => "编辑模型",
            Self::Model(None) => "新增模型",
            Self::LogProfile(Some(_)) => "编辑日志类型",
            Self::LogProfile(None) => "新增日志类型",
            Self::SystemPrompt => "默认系统提示词",
        }
    }

    /// 返回适合当前表单密度的固定模态框尺寸。
    fn dialog_size(self) -> (f32, f32) {
        match self {
            Self::Model(_) => (AI_MODEL_EDITOR_WIDTH, AI_MODEL_EDITOR_HEIGHT),
            Self::LogProfile(_) => (AI_LOG_PROFILE_EDITOR_WIDTH, AI_LOG_PROFILE_EDITOR_HEIGHT),
            Self::SystemPrompt => (
                AI_SYSTEM_PROMPT_EDITOR_WIDTH,
                AI_SYSTEM_PROMPT_EDITOR_HEIGHT,
            ),
        }
    }
}

/// 模型能力探测在设置编辑器中的展示状态。
#[derive(Clone)]
enum CapabilityProbeState {
    /// 尚未执行或配置编辑后需要重新执行。
    Idle,
    /// 正在后台调用固定无日志探测请求。
    Testing,
    /// 最近一次探测成功。
    Succeeded(String),
    /// 最近一次探测失败。
    Failed(String),
}

/// 编辑器内可聚焦字段。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AiDraftField {
    /// 模型配置的用户可读名称。
    ModelName,
    /// OpenAI 兼容根地址。
    BaseUrl,
    /// 模型 ID。
    Model,
    /// 模型上下文窗口 Token 数。
    ContextWindow,
    /// 只写不回显的 API Key。
    ApiKey,
    /// 当前日志类型名称。
    ProfileName,
    /// 当前名称规则模式。
    MatcherPattern,
    /// 当前日志分析说明。
    ProfileDescription,
    /// 全局默认系统提示词。
    SystemPrompt,
}

/// 一条模型配置草稿；API Key 仅在当前对话框生命周期内存在。
#[derive(Clone)]
struct ModelDraft {
    /// 稳定 UUID。
    profile_id: String,
    /// 是否允许新会话选择。
    enabled: bool,
    /// 设置页和启动对话框展示名称。
    name: TextInputState,
    /// OpenAI 兼容端点。
    base_url: TextInputState,
    /// 服务端模型 ID。
    model: TextInputState,
    /// 模型上下文窗口 Token 数。
    context_window: TextInputState,
    /// 新 API Key；永不从系统凭据库回填。
    api_key: TextInputState,
}

/// 一条名称匹配规则草稿。
#[derive(Clone)]
struct MatcherDraft {
    /// 匹配目标。
    target: LogNameMatcherTarget,
    /// 匹配方式。
    mode: LogNameMatcherMode,
    /// 模式输入状态。
    pattern: TextInputState,
    /// 是否区分大小写。
    case_sensitive: bool,
}

/// 一条日志类型配置草稿。
#[derive(Clone)]
struct ProfileDraft {
    /// 稳定 UUID。
    profile_id: String,
    /// 是否启用。
    enabled: bool,
    /// 类型名称输入。
    name: TextInputState,
    /// 匹配优先级。
    priority: u16,
    /// 1～16 条 OR 关系名称规则。
    matchers: Vec<MatcherDraft>,
    /// 分析说明输入。
    description: TextInputState,
}

/// 完整 AI 配置草稿。
#[derive(Clone)]
struct AiSettingsDraft {
    /// 可供智能分析选择的模型配置草稿。
    models: Vec<ModelDraft>,
    /// 当前单项模型编辑索引。
    selected_model: Option<usize>,
    /// 是否授权必要原文。
    allow_raw_log_content: bool,
    /// 原文授权版本。
    consent_version: String,
    /// 模型请求超时秒数。
    request_timeout_seconds: u64,
    /// 用户可编辑的专业分析系统提示词。
    system_prompt: TextInputState,
    /// 日志类型配置草稿。
    profiles: Vec<ProfileDraft>,
    /// 当前选中日志类型。
    selected_profile: Option<usize>,
    /// 当前选中名称规则。
    selected_matcher: usize,
}

impl AiSettingsDraft {
    /// 从持久化配置建立不含 API Key 的编辑草稿。
    fn from_config(mut config: AiConfig, kind: AiSettingsEditorKind) -> Self {
        config.normalize();
        let models = config
            .model_profiles
            .into_iter()
            .map(ModelDraft::from_profile)
            .collect::<Vec<_>>();
        let profiles = config
            .log_profiles
            .into_iter()
            .map(ProfileDraft::from_profile)
            .collect::<Vec<_>>();
        let mut draft = Self {
            selected_model: None,
            models,
            allow_raw_log_content: config.allow_raw_log_content,
            consent_version: config.consent_version,
            request_timeout_seconds: config.request_timeout_seconds,
            system_prompt: TextInputState::from_value(config.system_prompt),
            selected_profile: None,
            selected_matcher: 0,
            profiles,
        };
        match kind {
            AiSettingsEditorKind::Model(Some(index)) if index < draft.models.len() => {
                draft.selected_model = Some(index);
            }
            AiSettingsEditorKind::Model(_) => {
                draft.models.push(ModelDraft::new());
                draft.selected_model = Some(draft.models.len() - 1);
            }
            AiSettingsEditorKind::LogProfile(Some(index)) if index < draft.profiles.len() => {
                draft.selected_profile = Some(index);
            }
            AiSettingsEditorKind::LogProfile(_) => {
                draft.profiles.push(ProfileDraft::new());
                draft.selected_profile = Some(draft.profiles.len() - 1);
            }
            AiSettingsEditorKind::SystemPrompt => {}
        }
        draft
    }

    /// 转换为持久化配置；字段校验由主应用保存入口统一执行。
    fn to_config(&self) -> AiConfig {
        AiConfig {
            base_url: String::new(),
            model: String::new(),
            model_profiles: self.models.iter().map(ModelDraft::to_profile).collect(),
            allow_raw_log_content: self.allow_raw_log_content,
            consent_version: self.consent_version.clone(),
            budget_profile: crate::config::ai_config::AiBudgetProfile::Balanced,
            request_timeout_seconds: self.request_timeout_seconds,
            system_prompt: self.system_prompt.value.clone(),
            log_profiles: self.profiles.iter().map(ProfileDraft::to_profile).collect(),
        }
    }
}

impl ModelDraft {
    /// 从持久化模型配置建立不含 API Key 的编辑草稿。
    fn from_profile(profile: AiModelProfile) -> Self {
        Self {
            profile_id: profile.profile_id,
            enabled: profile.enabled,
            name: TextInputState::from_value(profile.name),
            base_url: TextInputState::from_value(profile.base_url),
            model: TextInputState::from_value(profile.model),
            context_window: TextInputState::from_value(profile.context_window_tokens.to_string()),
            api_key: TextInputState::default(),
        }
    }

    /// 创建一条具有稳定 ID 的空白模型草稿。
    fn new() -> Self {
        Self::from_profile(AiModelProfile::new())
    }

    /// 转换为可规范化并持久化的模型配置。
    fn to_profile(&self) -> AiModelProfile {
        AiModelProfile {
            profile_id: self.profile_id.clone(),
            enabled: self.enabled,
            name: self.name.value.clone(),
            base_url: self.base_url.value.clone(),
            model: self.model.value.clone(),
            context_window_tokens: self.context_window.value.trim().parse().unwrap_or(0),
        }
    }
}

impl ProfileDraft {
    /// 从持久化日志类型配置构造草稿。
    fn from_profile(profile: LogTypeProfile) -> Self {
        Self {
            profile_id: profile.profile_id,
            enabled: profile.enabled,
            name: TextInputState::from_value(profile.name),
            priority: profile.priority,
            matchers: profile
                .matchers
                .into_iter()
                .map(|matcher| MatcherDraft {
                    target: matcher.target,
                    mode: matcher.mode,
                    pattern: TextInputState::from_value(matcher.pattern),
                    case_sensitive: matcher.case_sensitive,
                })
                .collect(),
            description: TextInputState::from_value(profile.description),
        }
    }

    /// 创建一条可编辑的新日志类型草稿。
    fn new() -> Self {
        Self {
            profile_id: uuid::Uuid::new_v4().to_string(),
            enabled: true,
            name: TextInputState::default(),
            priority: 100,
            matchers: vec![MatcherDraft {
                target: LogNameMatcherTarget::FileName,
                mode: LogNameMatcherMode::Contains,
                pattern: TextInputState::default(),
                case_sensitive: false,
            }],
            description: TextInputState::default(),
        }
    }

    /// 转换为持久化日志类型配置。
    fn to_profile(&self) -> LogTypeProfile {
        LogTypeProfile {
            profile_id: self.profile_id.clone(),
            enabled: self.enabled,
            name: self.name.value.clone(),
            priority: self.priority,
            matchers: self
                .matchers
                .iter()
                .map(|matcher| LogNameMatcher {
                    target: matcher.target,
                    mode: matcher.mode,
                    pattern: matcher.pattern.value.clone(),
                    case_sensitive: matcher.case_sensitive,
                })
                .collect(),
            description: self.description.value.clone(),
        }
    }
}

/// AI 设置编辑器子视图。
pub(crate) struct AiSettingsEditor {
    /// 主应用实体。
    app: Entity<ArgusApp>,
    /// 当前主题。
    theme: AppTheme,
    /// 当前对话框只展示模型配置或日志类型说明中的一种。
    kind: AiSettingsEditorKind,
    /// 可编辑配置草稿。
    draft: AiSettingsDraft,
    /// 对话框根焦点，用于支持 Escape 关闭和点击空白处收拢输入焦点。
    root_focus: FocusHandle,
    /// 每类输入的稳定焦点句柄。
    focus_handles: AiSettingsFocusHandles,
    /// 分析说明多行滚动状态。
    description_scroll: ScrollHandle,
    /// 分析说明滚动条交互状态。
    description_scroll_state: TextareaScrollState,
    /// 默认系统提示词多行滚动状态。
    system_prompt_scroll: ScrollHandle,
    /// 默认系统提示词滚动条交互状态。
    system_prompt_scroll_state: TextareaScrollState,
    /// 模型配置正文纵向滚动状态。
    model_scroll: ScrollHandle,
    /// 日志类型详情纵向滚动状态。
    profile_editor_scroll: ScrollHandle,
    /// 最近保存校验错误。
    error_message: Option<String>,
    /// 当前模型能力探测状态。
    capability_probe_state: CapabilityProbeState,
    /// 模型能力探测 generation；配置编辑或新探测会使旧异步结果失效。
    capability_probe_generation: usize,
    /// 主应用主题订阅。
    _app_observer: Subscription,
}

/// AI 编辑器输入焦点集合。
struct AiSettingsFocusHandles {
    model_name: FocusHandle,
    base_url: FocusHandle,
    model: FocusHandle,
    context_window: FocusHandle,
    api_key: FocusHandle,
    profile_name: FocusHandle,
    matcher_pattern: FocusHandle,
    profile_description: FocusHandle,
    system_prompt: FocusHandle,
}

impl AiSettingsFocusHandles {
    /// 按字段返回对应焦点句柄。
    fn for_field(&self, field: AiDraftField) -> FocusHandle {
        match field {
            AiDraftField::ModelName => self.model_name.clone(),
            AiDraftField::BaseUrl => self.base_url.clone(),
            AiDraftField::Model => self.model.clone(),
            AiDraftField::ContextWindow => self.context_window.clone(),
            AiDraftField::ApiKey => self.api_key.clone(),
            AiDraftField::ProfileName => self.profile_name.clone(),
            AiDraftField::MatcherPattern => self.matcher_pattern.clone(),
            AiDraftField::ProfileDescription => self.profile_description.clone(),
            AiDraftField::SystemPrompt => self.system_prompt.clone(),
        }
    }
}

impl AiSettingsEditor {
    /// 创建 AI 设置编辑器并观察主题变化。
    pub(crate) fn new(
        app: Entity<ArgusApp>,
        theme: AppTheme,
        config: AiConfig,
        kind: AiSettingsEditorKind,
        cx: &mut Context<Self>,
    ) -> Self {
        let _app_observer = cx.observe(&app, |editor, app_entity, cx| {
            let next_theme = app_entity.read_with(cx, |app, _| app.theme.clone());
            if editor.theme != next_theme {
                editor.theme = next_theme;
                cx.notify();
            }
        });
        Self {
            app,
            theme,
            kind,
            draft: AiSettingsDraft::from_config(config, kind),
            root_focus: cx.focus_handle(),
            focus_handles: AiSettingsFocusHandles {
                model_name: cx.focus_handle(),
                base_url: cx.focus_handle(),
                model: cx.focus_handle(),
                context_window: cx.focus_handle(),
                api_key: cx.focus_handle(),
                profile_name: cx.focus_handle(),
                matcher_pattern: cx.focus_handle(),
                profile_description: cx.focus_handle(),
                system_prompt: cx.focus_handle(),
            },
            description_scroll: ScrollHandle::new(),
            description_scroll_state: TextareaScrollState::new(),
            system_prompt_scroll: ScrollHandle::new(),
            system_prompt_scroll_state: TextareaScrollState::new(),
            model_scroll: ScrollHandle::new(),
            profile_editor_scroll: ScrollHandle::new(),
            error_message: None,
            capability_probe_state: CapabilityProbeState::Idle,
            capability_probe_generation: 0,
            _app_observer,
        }
    }

    /// 返回指定字段可变输入状态。
    fn input_mut(&mut self, field: AiDraftField) -> Option<&mut TextInputState> {
        match field {
            AiDraftField::ModelName => self.selected_model_mut().map(|model| &mut model.name),
            AiDraftField::BaseUrl => self.selected_model_mut().map(|model| &mut model.base_url),
            AiDraftField::Model => self.selected_model_mut().map(|model| &mut model.model),
            AiDraftField::ContextWindow => self
                .selected_model_mut()
                .map(|model| &mut model.context_window),
            AiDraftField::ApiKey => self.selected_model_mut().map(|model| &mut model.api_key),
            AiDraftField::ProfileName => {
                self.selected_profile_mut().map(|profile| &mut profile.name)
            }
            AiDraftField::MatcherPattern => self
                .selected_matcher_mut()
                .map(|matcher| &mut matcher.pattern),
            AiDraftField::ProfileDescription => self
                .selected_profile_mut()
                .map(|profile| &mut profile.description),
            AiDraftField::SystemPrompt => Some(&mut self.draft.system_prompt),
        }
    }

    /// 返回当前单项模型可变草稿。
    fn selected_model_mut(&mut self) -> Option<&mut ModelDraft> {
        self.draft
            .selected_model
            .and_then(|index| self.draft.models.get_mut(index))
    }

    /// 返回当前日志类型可变草稿。
    fn selected_profile_mut(&mut self) -> Option<&mut ProfileDraft> {
        self.draft
            .selected_profile
            .and_then(|index| self.draft.profiles.get_mut(index))
    }

    /// 返回当前名称规则可变草稿。
    fn selected_matcher_mut(&mut self) -> Option<&mut MatcherDraft> {
        let matcher_index = self.draft.selected_matcher;
        self.selected_profile_mut()
            .and_then(|profile| profile.matchers.get_mut(matcher_index))
    }

    /// 清理全部字段的业务焦点并聚焦目标字段。
    fn focus_field(&mut self, field: AiDraftField) {
        for candidate in [
            AiDraftField::ModelName,
            AiDraftField::BaseUrl,
            AiDraftField::Model,
            AiDraftField::ContextWindow,
            AiDraftField::ApiKey,
            AiDraftField::ProfileName,
            AiDraftField::MatcherPattern,
            AiDraftField::ProfileDescription,
            AiDraftField::SystemPrompt,
        ] {
            if let Some(input) = self.input_mut(candidate) {
                input.is_focused = candidate == field;
                if candidate != field {
                    input.marked_range = None;
                    input.selection_drag = None;
                }
            }
        }
    }

    /// 处理输入字段按键。
    fn handle_input_key(
        &mut self,
        field: AiDraftField,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) {
        let multiline = matches!(
            field,
            AiDraftField::ProfileDescription | AiDraftField::SystemPrompt
        );
        let Some(input) = self.input_mut(field) else {
            return;
        };
        match handle_local_input_key(input, &event.keystroke, multiline, cx) {
            LocalInputAction::Close => self.close(cx),
            LocalInputAction::Changed | LocalInputAction::Submit => {
                self.error_message = None;
                if matches!(
                    field,
                    AiDraftField::ModelName
                        | AiDraftField::BaseUrl
                        | AiDraftField::Model
                        | AiDraftField::ApiKey
                ) {
                    self.invalidate_capability_probe();
                }
            }
            LocalInputAction::None => {}
        }
    }

    /// 新增当前日志类型的 OR 匹配规则。
    fn add_matcher(&mut self) {
        let Some(profile) = self.selected_profile_mut() else {
            return;
        };
        if profile.matchers.len() >= 16 {
            self.error_message = Some("每个日志类型最多配置 16 条名称规则".to_string());
            return;
        }
        profile.matchers.push(MatcherDraft {
            target: LogNameMatcherTarget::FileName,
            mode: LogNameMatcherMode::Contains,
            pattern: TextInputState::default(),
            case_sensitive: false,
        });
        self.draft.selected_matcher = profile.matchers.len() - 1;
    }

    /// 删除当前名称规则，至少保留一条。
    fn remove_matcher(&mut self) {
        let index = self.draft.selected_matcher;
        let Some(profile) = self.selected_profile_mut() else {
            return;
        };
        if profile.matchers.len() <= 1 {
            self.error_message = Some("每个日志类型至少保留一条名称规则".to_string());
            return;
        }
        profile
            .matchers
            .remove(index.min(profile.matchers.len() - 1));
        self.draft.selected_matcher = index.min(profile.matchers.len() - 1);
    }

    /// 保存全部配置；API Key 为空表示保留系统凭据库中的已有值。
    fn save(&mut self, cx: &mut Context<Self>) {
        let config = self.draft.to_config();
        let credential = self
            .draft
            .selected_model
            .and_then(|index| self.draft.models.get(index))
            .map(|model| (model.base_url.value.clone(), model.api_key.value.clone()));
        let result = self.app.update(cx, |app, app_cx| {
            let result = app.save_ai_settings(config, credential);
            app_cx.notify();
            result
        });
        if let Err(message) = result {
            self.error_message = Some(message);
        }
    }

    /// 在专用 Tokio 运行时执行无日志能力探测，并把归一化结果回传当前编辑器。
    fn test_model_connection(&mut self, cx: &mut Context<Self>) {
        if matches!(self.capability_probe_state, CapabilityProbeState::Testing) {
            return;
        }
        let mut config = self.draft.to_config();
        config.allow_raw_log_content = false;
        config.consent_version.clear();
        config.log_profiles.clear();
        config.normalize();
        let Some(model_index) = self.draft.selected_model else {
            self.capability_probe_state =
                CapabilityProbeState::Failed("当前没有可测试的模型配置".to_string());
            return;
        };
        let Some(model) = config.model_profiles.get(model_index).cloned() else {
            self.capability_probe_state =
                CapabilityProbeState::Failed("当前模型配置已不存在".to_string());
            return;
        };
        if let Err(error) = model.validate() {
            self.capability_probe_state = CapabilityProbeState::Failed(error);
            return;
        }
        let entered_api_key = self
            .draft
            .models
            .get(model_index)
            .map(|model| model.api_key.value.trim().to_string())
            .unwrap_or_default();
        let credential_base_url = model.base_url.clone();
        let (result_sender, result_receiver) = async_channel::bounded(1);
        self.capability_probe_generation = self.capability_probe_generation.wrapping_add(1);
        let probe_generation = self.capability_probe_generation;
        self.capability_probe_state = CapabilityProbeState::Testing;
        crate::agent::agent_runtime().spawn(async move {
            let api_key_result = if entered_api_key.is_empty() {
                tokio::task::spawn_blocking(move || {
                    crate::agent::load_api_key(&credential_base_url)
                })
                .await
                .map_err(|_| "读取系统凭据库任务异常结束".to_string())
                .and_then(|result| result)
            } else {
                Ok(SecretString::from(entered_api_key))
            };
            let result = match api_key_result {
                Ok(api_key) => {
                    crate::agent::probe_model_capabilities(&config, &model, &api_key).await
                }
                Err(error) => Err(error),
            };
            let _ = result_sender.send(result).await;
        });
        cx.spawn(async move |view, cx| {
            let Ok(result) = result_receiver.recv().await else {
                return;
            };
            view.update(cx, |editor, cx| {
                if editor.capability_probe_generation != probe_generation {
                    return;
                }
                editor.capability_probe_state = match result {
                    Ok(message) => CapabilityProbeState::Succeeded(message),
                    Err(message) => CapabilityProbeState::Failed(message),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 使正在执行或已经完成的旧能力探测失效，禁止旧端点结果覆盖当前草稿。
    fn invalidate_capability_probe(&mut self) {
        self.capability_probe_generation = self.capability_probe_generation.wrapping_add(1);
        self.capability_probe_state = CapabilityProbeState::Idle;
    }

    /// 删除设置列表中打开的现有单项，并复用主应用持久化与校验入口。
    fn delete_existing(&mut self, cx: &mut Context<Self>) {
        match self.kind {
            AiSettingsEditorKind::Model(Some(index)) if index < self.draft.models.len() => {
                self.draft.models.remove(index);
            }
            AiSettingsEditorKind::LogProfile(Some(index)) if index < self.draft.profiles.len() => {
                self.draft.profiles.remove(index);
            }
            _ => return,
        }
        let config = self.draft.to_config();
        let result = self.app.update(cx, |app, app_cx| {
            let result = app.save_ai_settings(config, None);
            app_cx.notify();
            result
        });
        if let Err(message) = result {
            self.error_message = Some(message);
        }
    }

    /// 关闭编辑器且不保存草稿。
    fn close(&self, cx: &mut Context<Self>) {
        self.app.update(cx, |app, app_cx| {
            app.close_ai_settings_editor();
            app_cx.notify();
        });
    }
}

impl Render for AiSettingsEditor {
    /// 按当前类型渲染模型配置或日志类型说明，并复用统一标题栏与底部操作区。
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let escape_entity = entity.clone();
        let header_close_entity = entity.clone();
        let cancel_entity = entity.clone();
        let save_entity = entity.clone();
        let delete_entity = entity.clone();
        let theme = self.theme.clone();
        let draft = self.draft.clone();
        let kind = self.kind;
        let body = match kind {
            AiSettingsEditorKind::Model(_) => {
                render_model_configuration(self, entity.clone(), &draft, &theme)
            }
            AiSettingsEditorKind::LogProfile(_) => {
                render_log_profile_configuration(self, entity.clone(), &draft, &theme)
            }
            AiSettingsEditorKind::SystemPrompt => {
                render_system_prompt_configuration(self, entity.clone(), &draft, &theme)
            }
        };

        div()
            .id("ai-settings-editor-root")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded_lg()
            .bg(rgb(theme.content))
            .border_1()
            .border_color(rgb(theme.border))
            .font_family(ARGUS_UI_FONT_FAMILY)
            .text_color(rgb(theme.foreground))
            .occlude()
            .focusable()
            .track_focus(&self.root_focus)
            .on_key_down(move |event: &KeyDownEvent, _, app_cx| {
                if event.keystroke.key == "escape" {
                    app_cx.stop_propagation();
                    escape_entity.update(app_cx, |editor, cx| editor.close(cx));
                }
            })
            .child(
                div()
                    .h(px(AI_EDITOR_HEADER_HEIGHT))
                    .flex_none()
                    .px_5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(render_icon(
                                match kind {
                                    AiSettingsEditorKind::Model(_) => ArgusIcon::Settings,
                                    AiSettingsEditorKind::LogProfile(_) => ArgusIcon::Logs,
                                    AiSettingsEditorKind::SystemPrompt => ArgusIcon::FileText,
                                },
                                theme.foreground_muted,
                                16.0,
                            ))
                            .child(
                                div()
                                    .text_size(px(14.0))
                                    .line_height(px(18.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(kind.dialog_title()),
                            ),
                    )
                    .child(render_icon_button(
                        "ai-settings-close",
                        ArgusIcon::Close,
                        "关闭配置",
                        false,
                        IconButtonSize::Small,
                        &theme,
                        move |_, _, app_cx| {
                            app_cx.stop_propagation();
                            header_close_entity.update(app_cx, |editor, cx| editor.close(cx));
                        },
                    )),
            )
            .child(body)
            .child(
                div()
                    .h(px(AI_EDITOR_FOOTER_HEIGHT))
                    .flex_none()
                    .px_5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .min_w(px(0.0))
                            .flex_1()
                            .mr_4()
                            .truncate()
                            .text_size(px(11.0))
                            .text_color(rgb(if self.error_message.is_some() {
                                theme.error
                            } else {
                                theme.foreground_muted
                            }))
                            .child(self.error_message.clone().unwrap_or_else(|| {
                                match kind {
                                    AiSettingsEditorKind::Model(_) => {
                                        "API Key 只写入系统凭据库，留空将保留已有密钥".to_string()
                                    }
                                    AiSettingsEditorKind::LogProfile(_) => {
                                        "名称规则按优先级匹配，日志说明会发送给所配置的模型服务"
                                            .to_string()
                                    }
                                    AiSettingsEditorKind::SystemPrompt => {
                                        "提示词只补充专业角色，不能覆盖固定流程和安全规则"
                                            .to_string()
                                    }
                                }
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(
                                matches!(
                                    kind,
                                    AiSettingsEditorKind::Model(Some(_))
                                        | AiSettingsEditorKind::LogProfile(Some(_))
                                ),
                                |this| {
                                    this.child(editor_action_button(
                                        "ai-settings-delete",
                                        ArgusIcon::Trash,
                                        "删除",
                                        false,
                                        &theme,
                                        move |_, _, app_cx| {
                                            app_cx.stop_propagation();
                                            delete_entity.update(app_cx, |editor, cx| {
                                                editor.delete_existing(cx);
                                                cx.notify();
                                            });
                                        },
                                    ))
                                },
                            )
                            .child(editor_action_button(
                                "ai-settings-cancel",
                                ArgusIcon::Close,
                                "取消",
                                false,
                                &theme,
                                move |_, _, app_cx| {
                                    app_cx.stop_propagation();
                                    cancel_entity.update(app_cx, |editor, cx| editor.close(cx));
                                },
                            ))
                            .child(editor_action_button(
                                "ai-settings-save",
                                ArgusIcon::Save,
                                "保存",
                                true,
                                &theme,
                                move |_, _, app_cx| {
                                    app_cx.stop_propagation();
                                    save_entity.update(app_cx, |editor, cx| {
                                        editor.save(cx);
                                        cx.notify();
                                    });
                                },
                            )),
                    ),
            )
    }
}

/// 渲染模型服务、能力探测和日志原文授权配置。
fn render_model_configuration(
    editor: &AiSettingsEditor,
    entity: Entity<AiSettingsEditor>,
    draft: &AiSettingsDraft,
    theme: &AppTheme,
) -> AnyElement {
    let Some(model_index) = draft.selected_model else {
        return div().into_any_element();
    };
    let Some(model) = draft.models.get(model_index) else {
        return div().into_any_element();
    };
    let toggle_model = entity.clone();
    let toggle_raw = entity.clone();
    let test_connection = entity.clone();
    let scroll_handle = editor.model_scroll.clone();
    let content = div()
        .id("ai-model-editor-scroll")
        .size_full()
        .overflow_y_scroll()
        .scrollbar_width(px(AI_EDITOR_SCROLLBAR_WIDTH))
        .track_scroll(&scroll_handle)
        .p_5()
        .child(
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap_4()
                .child(editor_section_heading(
                    "模型服务",
                    "配置兼容 OpenAI Chat Completions 工具调用的模型服务。",
                    theme,
                ))
                .child(
                    editor_card(theme)
                        .child(toggle_row(
                            "允许选择该模型",
                            model.enabled,
                            theme,
                            move |_, _, app_cx| {
                                toggle_model.update(app_cx, move |editor, cx| {
                                    if let Some(model) = editor.draft.models.get_mut(model_index) {
                                        model.enabled = !model.enabled;
                                    }
                                    editor.error_message = None;
                                    cx.notify();
                                });
                            },
                        ))
                        .child(labeled_input(
                            "配置名称",
                            "用于设置列表和发起智能分析时选择模型。",
                            render_local_input(
                                entity.clone(),
                                AiDraftField::ModelName,
                                &model.name,
                                false,
                                "例如：生产环境 GPT",
                                &editor.focus_handles,
                                theme,
                            ),
                            theme,
                        ))
                        .child(labeled_input(
                            "服务地址",
                            "外部服务必须使用 HTTPS，本机服务允许 HTTP。",
                            render_local_input(
                                entity.clone(),
                                AiDraftField::BaseUrl,
                                &model.base_url,
                                false,
                                "例如 https://host/v1",
                                &editor.focus_handles,
                                theme,
                            ),
                            theme,
                        ))
                        .child(labeled_input(
                            "模型 ID",
                            "填写服务端支持结构化工具调用的模型标识。",
                            render_local_input(
                                entity.clone(),
                                AiDraftField::Model,
                                &model.model,
                                false,
                                "模型 ID",
                                &editor.focus_handles,
                                theme,
                            ),
                            theme,
                        ))
                        .child(labeled_input(
                            "上下文大小",
                            "填写模型上下文窗口的 Token 数，用于分析时计算当前请求占用比例。",
                            render_local_input(
                                entity.clone(),
                                AiDraftField::ContextWindow,
                                &model.context_window,
                                false,
                                "例如 128000",
                                &editor.focus_handles,
                                theme,
                            ),
                            theme,
                        ))
                        .child(labeled_input(
                            "API Key",
                            "密钥不会写入 settings.toml；留空表示保留系统凭据库中的已有值。",
                            render_local_input(
                                entity.clone(),
                                AiDraftField::ApiKey,
                                &model.api_key,
                                true,
                                "API Key（留空保留已有密钥）",
                                &editor.focus_handles,
                                theme,
                            ),
                            theme,
                        ))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(editor_button(
                                    "ai-test-connection",
                                    if matches!(
                                        editor.capability_probe_state,
                                        CapabilityProbeState::Testing
                                    ) {
                                        "测试中…"
                                    } else {
                                        "测试连接与工具调用"
                                    },
                                    false,
                                    theme,
                                    move |_, _, app_cx| {
                                        test_connection.update(app_cx, |editor, cx| {
                                            editor.test_model_connection(cx);
                                            cx.notify();
                                        });
                                    },
                                ))
                                .child(
                                    div()
                                        .min_w(px(0.0))
                                        .flex_1()
                                        .text_size(px(11.0))
                                        .text_color(rgb(match &editor.capability_probe_state {
                                            CapabilityProbeState::Succeeded(_) => theme.info,
                                            CapabilityProbeState::Failed(_) => theme.error,
                                            _ => theme.foreground_muted,
                                        }))
                                        .child(match &editor.capability_probe_state {
                                            CapabilityProbeState::Idle => "尚未探测".to_string(),
                                            CapabilityProbeState::Testing => {
                                                "正在发送固定无日志请求".to_string()
                                            }
                                            CapabilityProbeState::Succeeded(message)
                                            | CapabilityProbeState::Failed(message) => {
                                                message.clone()
                                            }
                                        }),
                                ),
                        ),
                )
                .child(editor_section_heading(
                    "数据发送",
                    "智能分析默认只使用来源元数据和本地聚合结果。",
                    theme,
                ))
                .child(
                    editor_card(theme)
                        .child(toggle_row(
                            "允许发送必要日志原文",
                            draft.allow_raw_log_content,
                            theme,
                            move |_, _, app_cx| {
                                toggle_raw.update(app_cx, |editor, cx| {
                                    editor.draft.allow_raw_log_content =
                                        !editor.draft.allow_raw_log_content;
                                    editor.draft.consent_version =
                                        if editor.draft.allow_raw_log_content {
                                            AI_RAW_LOG_CONSENT_VERSION.to_string()
                                        } else {
                                            String::new()
                                        };
                                    editor.error_message = None;
                                    cx.notify();
                                });
                            },
                        ))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(rgb(theme.warning))
                                .child("启用后，仅允许结构化工具把经过裁剪、脱敏且受预算限制的必要片段发送到当前模型服务。"),
                        ),
                ),
        );
    div()
        .flex_1()
        .min_h(px(0.0))
        .relative()
        .child(content)
        .child(render_editor_vertical_scrollbar(
            &scroll_handle,
            theme,
            entity,
        ))
        .into_any_element()
}

/// 渲染从设置列表打开的单项日志类型新增或编辑表单。
fn render_log_profile_configuration(
    editor: &AiSettingsEditor,
    entity: Entity<AiSettingsEditor>,
    draft: &AiSettingsDraft,
    theme: &AppTheme,
) -> AnyElement {
    let profile_editor_scroll = editor.profile_editor_scroll.clone();
    div()
        .flex_1()
        .min_h(px(0.0))
        .relative()
        .child(
            div()
                .id("ai-profile-editor-scroll")
                .size_full()
                .overflow_y_scroll()
                .scrollbar_width(px(AI_EDITOR_SCROLLBAR_WIDTH))
                .track_scroll(&profile_editor_scroll)
                .p_5()
                .child(render_profile_editor(editor, entity.clone(), draft, theme)),
        )
        .child(render_editor_vertical_scrollbar(
            &profile_editor_scroll,
            theme,
            entity,
        ))
        .into_any_element()
}

/// 渲染当前日志类型和名称规则编辑区。
fn render_profile_editor(
    editor: &AiSettingsEditor,
    entity: Entity<AiSettingsEditor>,
    draft: &AiSettingsDraft,
    theme: &AppTheme,
) -> AnyElement {
    let Some(profile_index) = draft.selected_profile else {
        return div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(12.0))
            .text_color(rgb(theme.foreground_muted))
            .child("新增或选择一个日志类型以编辑名称规则和分析说明")
            .into_any_element();
    };
    let Some(profile) = draft.profiles.get(profile_index) else {
        return div().into_any_element();
    };
    let selected_matcher = profile.matchers.get(draft.selected_matcher);
    let toggle_profile = entity.clone();
    let priority_down = entity.clone();
    let priority_up = entity.clone();
    let add_matcher = entity.clone();
    let remove_matcher = entity.clone();
    let cycle_target = entity.clone();
    let cycle_mode = entity.clone();
    let toggle_case = entity.clone();

    div()
        .w_full()
        .min_w(px(0.0))
        .flex()
        .flex_col()
        .gap_4()
        .child(editor_section_heading(
            "基本信息",
            "日志类型名称会显示在分析轨迹和结构化报告中。",
            theme,
        ))
        .child(
            editor_card(theme)
                .child(toggle_row(
                    "启用该配置",
                    profile.enabled,
                    theme,
                    move |_, _, app_cx| {
                        toggle_profile.update(app_cx, move |editor, cx| {
                            if let Some(profile) =
                                editor.draft.profiles.get_mut(profile_index)
                            {
                                profile.enabled = !profile.enabled;
                            }
                            cx.notify();
                        });
                    },
                ))
                .child(labeled_input(
                    "日志类型名称",
                    "长度为 1～64 个字符。",
                    render_local_input(
                        entity.clone(),
                        AiDraftField::ProfileName,
                        &profile.name,
                        false,
                        "例如：网关访问日志",
                        &editor.focus_handles,
                        theme,
                    ),
                    theme,
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .text_size(px(11.0))
                        .child(format!(
                            "优先级：{}（数值越大越优先）",
                            profile.priority
                        ))
                        .child(
                            div()
                                .flex()
                                .gap_1()
                                .child(editor_button(
                                    "ai-priority-down",
                                    "-10",
                                    false,
                                    theme,
                                    move |_, _, app_cx| {
                                        priority_down.update(app_cx, move |editor, cx| {
                                            if let Some(profile) =
                                                editor.draft.profiles.get_mut(profile_index)
                                            {
                                                profile.priority =
                                                    profile.priority.saturating_sub(10);
                                            }
                                            cx.notify();
                                        });
                                    },
                                ))
                                .child(editor_button(
                                    "ai-priority-up",
                                    "+10",
                                    false,
                                    theme,
                                    move |_, _, app_cx| {
                                        priority_up.update(app_cx, move |editor, cx| {
                                            if let Some(profile) =
                                                editor.draft.profiles.get_mut(profile_index)
                                            {
                                                profile.priority = profile
                                                    .priority
                                                    .saturating_add(10)
                                                    .min(1000);
                                            }
                                            cx.notify();
                                        });
                                    },
                                )),
                        ),
                ),
        )
        .child(editor_section_heading(
            "名称匹配规则",
            "任意一条规则命中即采用该日志类型；多配置命中时按优先级选择。",
            theme,
        ))
        .child(
            editor_card(theme)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(section_title("匹配规则（OR）", theme))
                        .child(
                            div()
                                .flex()
                                .gap_1()
                                .child(editor_button(
                                    "ai-matcher-add",
                                    "新增规则",
                                    false,
                                    theme,
                                    move |_, _, app_cx| {
                                        add_matcher.update(app_cx, |editor, cx| {
                                            editor.add_matcher();
                                            cx.notify();
                                        });
                                    },
                                ))
                                .child(editor_button(
                                    "ai-matcher-remove",
                                    "删除规则",
                                    false,
                                    theme,
                                    move |_, _, app_cx| {
                                        remove_matcher.update(app_cx, |editor, cx| {
                                            editor.remove_matcher();
                                            cx.notify();
                                        });
                                    },
                                )),
                        ),
                )
                .child(
                    div()
                        .id("ai-matcher-tabs-scroll")
                        .w_full()
                        .overflow_x_scroll()
                        .scrollbar_width(px(6.0))
                        .pb_2()
                        .child(
                            div().flex().gap_1().children(profile.matchers.iter().enumerate().map(
                                |(index, _)| {
                                    let select = entity.clone();
                                    editor_button_dynamic(
                                        ("ai-matcher-tab", index),
                                        format!("规则 {}", index + 1),
                                        draft.selected_matcher == index,
                                        theme,
                                        move |_, _, app_cx| {
                                            select.update(app_cx, |editor, cx| {
                                                editor.draft.selected_matcher = index;
                                                cx.notify();
                                            });
                                        },
                                    )
                                },
                            )),
                        ),
                )
                .when_some(selected_matcher.cloned(), |this, matcher| {
                    this.child(
                        div()
                            .flex()
                            .gap_2()
                            .child(editor_button_dynamic(
                                "ai-matcher-target",
                                format!("目标：{}", matcher_target_label(matcher.target)),
                                false,
                                theme,
                                move |_, _, app_cx| {
                                    cycle_target.update(app_cx, |editor, cx| {
                                        if let Some(matcher) = editor.selected_matcher_mut() {
                                            matcher.target = match matcher.target {
                                                LogNameMatcherTarget::FileName => {
                                                    LogNameMatcherTarget::RelativePath
                                                }
                                                LogNameMatcherTarget::RelativePath => {
                                                    LogNameMatcherTarget::FileName
                                                }
                                            };
                                        }
                                        cx.notify();
                                    });
                                },
                            ))
                            .child(editor_button_dynamic(
                                "ai-matcher-mode",
                                format!("方式：{}", matcher_mode_label(matcher.mode)),
                                false,
                                theme,
                                move |_, _, app_cx| {
                                    cycle_mode.update(app_cx, |editor, cx| {
                                        if let Some(matcher) = editor.selected_matcher_mut() {
                                            matcher.mode = next_matcher_mode(matcher.mode);
                                        }
                                        cx.notify();
                                    });
                                },
                            ))
                            .child(editor_button_dynamic(
                                "ai-matcher-case",
                                if matcher.case_sensitive {
                                    "区分大小写"
                                } else {
                                    "忽略大小写"
                                }
                                .to_string(),
                                matcher.case_sensitive,
                                theme,
                                move |_, _, app_cx| {
                                    toggle_case.update(app_cx, |editor, cx| {
                                        if let Some(matcher) = editor.selected_matcher_mut() {
                                            matcher.case_sensitive = !matcher.case_sensitive;
                                        }
                                        cx.notify();
                                    });
                                },
                            )),
                    )
                    .child(labeled_input(
                        "名称或路径模式",
                        "支持完全匹配、前缀、后缀、包含和 Rust 正则。",
                        render_local_input(
                            entity.clone(),
                            AiDraftField::MatcherPattern,
                            &matcher.pattern,
                            false,
                            "名称文本或 Rust 正则（1～512 字符）",
                            &editor.focus_handles,
                            theme,
                        ),
                        theme,
                    ))
                }),
        )
        .child(editor_section_heading(
            "日志分析说明",
            "说明只提供业务语义，不能改变 Agent 的工具权限和证据要求。",
            theme,
        ))
        .child(
            editor_card(theme)
                .child(render_local_textarea(
                    entity,
                    AiDraftField::ProfileDescription,
                    &profile.description,
                    editor
                        .focus_handles
                        .for_field(AiDraftField::ProfileDescription),
                    editor.description_scroll.clone(),
                    editor.description_scroll_state.clone(),
                    "ai-profile-description",
                    "说明日志来源、字段含义、关联方式、常见故障和分析建议（1 B～4 KiB）",
                    7,
                    theme,
                ))
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(rgb(theme.foreground_muted))
                        .child("建议填写字段含义、时区、关联 ID、已知故障特征和误判；不要填写密码、Token 或要求执行命令。"),
                ),
        )
        .into_any_element()
}

/// 渲染全局默认系统提示词编辑页。
///
/// 可编辑内容只作为低优先级专业指令注入；编排器中的固定分析流程、最高思考模式、工具权限和
/// 强制证据校验不在此处展示或开放修改，避免配置误操作削弱安全边界。
fn render_system_prompt_configuration(
    editor: &AiSettingsEditor,
    entity: Entity<AiSettingsEditor>,
    draft: &AiSettingsDraft,
    theme: &AppTheme,
) -> AnyElement {
    let reset_entity = entity.clone();
    div()
        .flex_1()
        .min_h(px(0.0))
        .p_5()
        .flex()
        .flex_col()
        .gap_4()
        .child(editor_section_heading(
            "专业分析提示",
            "用于补充角色、领域知识和表达偏好；新安装与旧配置会自动采用内置默认值。",
            theme,
        ))
        .child(
            editor_card(theme)
                .flex_1()
                .min_h(px(0.0))
                .child(render_local_textarea(
                    entity,
                    AiDraftField::SystemPrompt,
                    &draft.system_prompt,
                    editor.focus_handles.for_field(AiDraftField::SystemPrompt),
                    editor.system_prompt_scroll.clone(),
                    editor.system_prompt_scroll_state.clone(),
                    "ai-system-prompt",
                    "填写专业角色、领域知识和输出偏好",
                    16,
                    theme,
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .min_w(px(0.0))
                                .flex_1()
                                .text_size(px(10.0))
                                .text_color(rgb(theme.foreground_muted))
                                .child("最多 32 KiB。固定分析流程、工具沙箱、最高思考模式和证据校验不可被此提示词关闭。"),
                        )
                        .child(editor_button(
                            "ai-system-prompt-reset",
                            "恢复默认",
                            false,
                            theme,
                            move |_, _, app_cx| {
                                reset_entity.update(app_cx, |editor, cx| {
                                    editor.draft.system_prompt =
                                        TextInputState::from_value(DEFAULT_AI_SYSTEM_PROMPT.to_string());
                                    editor.error_message = None;
                                    cx.notify();
                                });
                            },
                        )),
                ),
        )
        .into_any_element()
}

/// 渲染本地状态单行输入框。
fn render_local_input(
    entity: Entity<AiSettingsEditor>,
    field: AiDraftField,
    input: &TextInputState,
    secret: bool,
    placeholder: &'static str,
    focus_handles: &AiSettingsFocusHandles,
    theme: &AppTheme,
) -> AnyElement {
    let native_entity = entity.clone();
    let key_entity = entity.clone();
    let click_entity = entity.clone();
    let pointer_entity = entity.clone();
    let focus_handle = focus_handles.for_field(field);
    let native_input = NativeInput::new(focus_handle.clone(), move |edit, _, app_cx| {
        native_entity.update(app_cx, |editor, cx| {
            editor.focus_field(field);
            if let Some(input) = editor.input_mut(field) {
                input.apply_native_edit(&edit);
            }
            editor.error_message = None;
            if matches!(
                field,
                AiDraftField::BaseUrl | AiDraftField::Model | AiDraftField::ApiKey
            ) {
                editor.invalidate_capability_probe();
            }
            cx.notify();
        });
    });
    render_input(
        Input {
            id: input_id(field),
            placeholder,
            value: input.value.clone(),
            is_disabled: false,
            is_focused: input.is_focused,
            cursor_index: input.cursor,
            selection_range: input.selection_range(),
            marked_range: input.marked_range.clone(),
            is_pointer_selecting: input.selection_drag.is_some(),
            is_secret: secret,
            size: InputSize::Regular,
            leading_accessory: None,
            trailing_accessory: if input.value.is_empty() {
                None
            } else {
                Some(InputAccessory {
                    id: "ai-input-value",
                    icon: ArgusIcon::Close,
                    tooltip: "清空",
                })
            },
            native_input: Some(native_input),
        },
        theme,
        move |event, _, app_cx| {
            app_cx.stop_propagation();
            key_entity.update(app_cx, |editor, cx| {
                editor.handle_input_key(field, event, cx);
                cx.notify();
            });
        },
        move |_, window, app_cx| {
            app_cx.stop_propagation();
            click_entity.update(app_cx, |editor, cx| {
                editor.focus_field(field);
                focus_handle.focus(window);
                cx.notify();
            });
        },
        move |event: &InputPointerEvent, _, app_cx| {
            pointer_entity.update(app_cx, |editor, cx| {
                editor.focus_field(field);
                if let Some(input) = editor.input_mut(field) {
                    apply_pointer_event(input, event);
                }
                cx.notify();
            });
        },
        move |_, _, app_cx| {
            entity.update(app_cx, |editor, cx| {
                if let Some(input) = editor.input_mut(field) {
                    *input = TextInputState::default();
                    input.is_focused = true;
                }
                if matches!(
                    field,
                    AiDraftField::BaseUrl | AiDraftField::Model | AiDraftField::ApiKey
                ) {
                    editor.invalidate_capability_probe();
                }
                cx.notify();
            });
        },
    )
    .into_any_element()
}

/// 渲染分析说明或系统提示词使用的本地状态多行输入框。
fn render_local_textarea(
    entity: Entity<AiSettingsEditor>,
    field: AiDraftField,
    input: &TextInputState,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    scroll_state: TextareaScrollState,
    id: &'static str,
    placeholder: &'static str,
    visible_lines: usize,
    theme: &AppTheme,
) -> AnyElement {
    let native_entity = entity.clone();
    let key_entity = entity.clone();
    let click_entity = entity.clone();
    let pointer_entity = entity.clone();
    let native_input = NativeInput::new(focus_handle.clone(), move |edit, _, app_cx| {
        native_entity.update(app_cx, |editor, cx| {
            editor.focus_field(field);
            if let Some(input) = editor.input_mut(field) {
                input.apply_native_edit(&edit);
            }
            editor.error_message = None;
            cx.notify();
        });
    });
    render_textarea(
        Textarea {
            id,
            placeholder,
            value: input.value.clone(),
            is_disabled: false,
            is_focused: input.is_focused,
            cursor_index: input.cursor,
            selection_range: input.selection_range(),
            marked_range: input.marked_range.clone(),
            is_pointer_selecting: input.selection_drag.is_some(),
            visible_lines,
            fill_height: false,
            scroll_handle,
            scroll_state,
            style: TextareaStyle::Default,
            trailing_accessory: None,
            trailing_accessory_position: TextareaAccessoryPosition::TopRight,
            trailing_accessory_always_visible: false,
            trailing_accessory_selected: false,
            native_input: Some(native_input),
        },
        theme,
        move |event, _, app_cx| {
            key_entity.update(app_cx, |editor, cx| {
                editor.handle_input_key(field, event, cx);
                cx.notify();
            });
        },
        move |_, window, app_cx| {
            click_entity.update(app_cx, |editor, cx| {
                editor.focus_field(field);
                focus_handle.focus(window);
                cx.notify();
            });
        },
        move |event, _, app_cx| {
            pointer_entity.update(app_cx, |editor, cx| {
                editor.focus_field(field);
                if let Some(input) = editor.input_mut(field) {
                    apply_pointer_event(input, event);
                }
                cx.notify();
            });
        },
        |_, _, _| {},
    )
    .into_any_element()
}

/// 应用通用输入组件已经换算好的鼠标字符位置。
fn apply_pointer_event(input: &mut TextInputState, event: &InputPointerEvent) {
    match event.action {
        InputPointerAction::Begin => {
            input.begin_pointer_selection(event.character_index, event.granularity)
        }
        InputPointerAction::Extend => input.update_pointer_selection(event.character_index),
        InputPointerAction::Finish => input.finish_pointer_selection(),
    }
}

/// 渲染 AI 设置对话框共用的只读纵向滚动条，并在首帧布局完成后触发一次重绘。
fn render_editor_vertical_scrollbar(
    scroll_handle: &ScrollHandle,
    theme: &AppTheme,
    entity: Entity<AiSettingsEditor>,
) -> AnyElement {
    let bounds = scroll_handle.bounds();
    let max_offset = scroll_handle.max_offset();
    let content_height = bounds.size.height + max_offset.height;
    if let Some(metrics) = scrollbar_metrics(
        bounds.size.height,
        content_height,
        -scroll_handle.offset().y,
        AI_EDITOR_SCROLLBAR_PADDING,
        AI_EDITOR_SCROLLBAR_MIN_THUMB,
    ) {
        return div()
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .w(px(AI_EDITOR_SCROLLBAR_WIDTH))
            .child(
                div()
                    .absolute()
                    .top(metrics.thumb_start)
                    .right(px(2.0))
                    .w(px(AI_EDITOR_SCROLLBAR_THUMB_WIDTH))
                    .h(metrics.thumb_length)
                    .rounded_lg()
                    .bg(rgb(theme.foreground_muted))
                    .opacity(0.48),
            )
            .into_any_element();
    }

    // `ScrollHandle` 的 bounds 和最大偏移在首帧 prepaint 后才可用；哨兵仅在确有溢出时通知下一帧。
    let sentinel_handle = scroll_handle.clone();
    canvas(
        |_, _, _| (),
        move |_, _, _, cx: &mut App| {
            if sentinel_handle.bounds().size.height > px(0.0)
                && sentinel_handle.max_offset().height > px(0.0)
            {
                cx.notify(entity.entity_id());
            }
        },
    )
    .absolute()
    .size_full()
    .into_any_element()
}

/// 将编辑器子视图包裹为主窗口顶层模态框。
pub(crate) fn render_ai_settings_editor_modal(
    editor: Entity<AiSettingsEditor>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    let (width, height) = editor.read(cx).kind.dialog_size();
    render_modal_dialog(
        ModalDialog {
            overlay_id: "ai-settings-editor-overlay",
            container_id: "ai-settings-editor-container",
            width,
            height,
            content: editor.into_any_element(),
        },
        theme.clone(),
        cx,
    )
    .into_any_element()
}

/// 渲染对话框内容区的分组标题和辅助说明。
fn editor_section_heading(
    title: &'static str,
    description: &'static str,
    theme: &AppTheme,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(13.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(theme.foreground))
                .child(title),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(rgb(theme.foreground_muted))
                .child(description),
        )
}

/// 创建与设置页设置行一致的分组卡片容器。
fn editor_card(theme: &AppTheme) -> gpui::Div {
    div()
        .w_full()
        .p_4()
        .flex()
        .flex_col()
        .gap_3()
        .rounded_sm()
        .bg(rgb(theme.current_line))
}

/// 为输入框补充稳定字段标题和说明，避免仅依赖占位文本表达含义。
fn labeled_input(
    label: &'static str,
    description: &'static str,
    input: impl IntoElement,
    theme: &AppTheme,
) -> impl IntoElement {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(theme.foreground))
                .child(label),
        )
        .child(input)
        .child(
            div()
                .text_size(px(10.0))
                .text_color(rgb(theme.foreground_muted))
                .child(description),
        )
}

/// 渲染对话框底部带图标的取消或保存按钮。
fn editor_action_button(
    id: &'static str,
    icon: ArgusIcon,
    label: &'static str,
    primary: bool,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .h(px(30.0))
        .when(primary, |this| this.px_4())
        .when(!primary, |this| this.px_3())
        .flex()
        .items_center()
        .justify_center()
        .gap_1()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(if primary {
            theme.selection
        } else {
            theme.current_line
        }))
        .text_size(px(12.0))
        .text_color(rgb(theme.foreground))
        .when(primary, |this| this.font_weight(FontWeight::SEMIBOLD))
        .cursor_pointer()
        .hover(|this| this.opacity(0.88))
        .child(render_icon(
            icon,
            if primary {
                theme.foreground
            } else {
                theme.foreground_muted
            },
            13.0,
        ))
        .child(label)
        .on_click(on_click)
}

/// 渲染设置区小标题。
fn section_title(label: &'static str, theme: &AppTheme) -> impl IntoElement {
    div()
        .text_size(px(12.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(theme.foreground))
        .child(label)
}

/// 渲染布尔设置行。
fn toggle_row(
    label: &'static str,
    enabled: bool,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(label)
        .h(px(30.0))
        .flex()
        .items_center()
        .justify_between()
        .text_size(px(11.0))
        .child(label)
        .child(render_icon(
            if enabled {
                ArgusIcon::ToggleRight
            } else {
                ArgusIcon::ToggleLeft
            },
            if enabled {
                theme.info
            } else {
                theme.foreground_muted
            },
            22.0,
        ))
        .cursor_pointer()
        .on_click(on_click)
}

/// 渲染固定文本小按钮。
fn editor_button(
    id: &'static str,
    label: &'static str,
    primary: bool,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    editor_button_dynamic(id, label, primary, theme, on_click)
}

/// 渲染动态 ID 和文本的小按钮。
fn editor_button_dynamic(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    primary: bool,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .h(px(28.0))
        .flex_none()
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
        .hover(|this| this.opacity(0.82))
        .child(label.into())
        .on_click(on_click)
}

/// 返回每个字段稳定输入 ID。
fn input_id(field: AiDraftField) -> &'static str {
    match field {
        AiDraftField::ModelName => "ai-model-name-input",
        AiDraftField::BaseUrl => "ai-base-url-input",
        AiDraftField::Model => "ai-model-input",
        AiDraftField::ContextWindow => "ai-model-context-window-input",
        AiDraftField::ApiKey => "ai-api-key-input",
        AiDraftField::ProfileName => "ai-profile-name-input",
        AiDraftField::MatcherPattern => "ai-matcher-pattern-input",
        AiDraftField::ProfileDescription => "ai-profile-description",
        AiDraftField::SystemPrompt => "ai-system-prompt",
    }
}

/// 返回名称匹配目标中文标签。
fn matcher_target_label(target: LogNameMatcherTarget) -> &'static str {
    match target {
        LogNameMatcherTarget::FileName => "文件名",
        LogNameMatcherTarget::RelativePath => "相对路径",
    }
}

/// 返回名称匹配方式中文标签。
fn matcher_mode_label(mode: LogNameMatcherMode) -> &'static str {
    match mode {
        LogNameMatcherMode::Exact => "完全相等",
        LogNameMatcherMode::Prefix => "前缀",
        LogNameMatcherMode::Suffix => "后缀",
        LogNameMatcherMode::Contains => "包含",
        LogNameMatcherMode::Regex => "正则",
    }
}

/// 按界面固定顺序轮换名称匹配方式。
fn next_matcher_mode(mode: LogNameMatcherMode) -> LogNameMatcherMode {
    match mode {
        LogNameMatcherMode::Exact => LogNameMatcherMode::Prefix,
        LogNameMatcherMode::Prefix => LogNameMatcherMode::Suffix,
        LogNameMatcherMode::Suffix => LogNameMatcherMode::Contains,
        LogNameMatcherMode::Contains => LogNameMatcherMode::Regex,
        LogNameMatcherMode::Regex => LogNameMatcherMode::Exact,
    }
}
