//! 文件职责：维护设置模态框和设置编辑器窗口的交互状态。
//! 创建日期：2026-06-12
//! 修改日期：2026-09-24
//! 作者：Argus 开发团队
//! 主要功能：维护设置模态框、智能分析设置和 Jstack 过滤规则编辑器，并统一持久化相关偏好。

use std::borrow::Borrow;
use std::ops::Range;

use gpui::{
    AppContext, Bounds, ClipboardItem, Context, Keystroke, WindowBounds, WindowOptions, px, size,
};

use crate::app::{
    AppTextInputTarget, ArgusApp, JstackAnalysisTaskState, JstackFilterRuleDraft,
    JstackThreadFilter, SettingsSection, TextInputState,
};
use crate::config::{JstackThreadFilterRule, JstackThreadFilterRuleKind};
use crate::infra::text_selection::{
    TextSelectionGranularity, character_count, insert_text_at_character_index,
    remove_character_range, slice_character_range,
};
use crate::platform::open_with_registration::{
    register_open_with, registration_status, unregister_open_with,
};
use crate::ui::settings_window::JstackFilterRuleEditorWindow;

/// Jstack 过滤规则编辑器默认宽度，给长堆栈保留横向阅读空间。
const JSTACK_FILTER_RULE_EDITOR_WIDTH: f32 = 920.0;
/// Jstack 过滤规则编辑器默认高度，避免大段规则配置挤在设置页小区域内。
const JSTACK_FILTER_RULE_EDITOR_HEIGHT: f32 = 680.0;
/// Jstack 过滤规则编辑器最小宽度。
const JSTACK_FILTER_RULE_EDITOR_MIN_WIDTH: f32 = 680.0;
/// Jstack 过滤规则编辑器最小高度。
const JSTACK_FILTER_RULE_EDITOR_MIN_HEIGHT: f32 = 520.0;

impl ArgusApp {
    /// 按设置列表索引打开模型编辑器；`None` 表示新增模型。
    pub(crate) fn open_ai_model_editor(
        &mut self,
        profile_index: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        if profile_index.is_none()
            && self.config.ai.model_profiles.len()
                >= crate::config::ai_config::MAX_AI_MODEL_PROFILE_COUNT
        {
            self.placeholder_notice = "最多配置 20 个模型".to_string();
            return;
        }
        self.open_ai_settings_editor(
            crate::ui::ai_settings_editor::AiSettingsEditorKind::Model(profile_index),
            cx,
        );
    }

    /// 按设置列表索引打开日志类型编辑器；`None` 表示新增日志类型。
    pub(crate) fn open_ai_log_profile_editor(
        &mut self,
        profile_index: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        if profile_index.is_none()
            && self.config.ai.log_profiles.len() >= crate::config::ai_config::MAX_LOG_PROFILE_COUNT
        {
            self.placeholder_notice = "最多配置 100 个日志类型".to_string();
            return;
        }
        self.open_ai_settings_editor(
            crate::ui::ai_settings_editor::AiSettingsEditorKind::LogProfile(profile_index),
            cx,
        );
    }

    /// 打开全局默认系统提示词编辑器。
    pub(crate) fn open_ai_system_prompt_editor(&mut self, cx: &mut Context<Self>) {
        self.open_ai_settings_editor(
            crate::ui::ai_settings_editor::AiSettingsEditorKind::SystemPrompt,
            cx,
        );
    }

    /// 按指定类型创建智能分析设置子对话框，并阻止多个编辑器相互覆盖草稿。
    fn open_ai_settings_editor(
        &mut self,
        kind: crate::ui::ai_settings_editor::AiSettingsEditorKind,
        cx: &mut Context<Self>,
    ) {
        if self.ai_settings_editor_modal.is_some() {
            self.placeholder_notice = "智能分析配置对话框已经打开".to_string();
            return;
        }
        let app = cx.entity();
        let theme = self.theme.clone();
        let config = self.config.ai.clone();
        self.ai_settings_editor_modal = Some(cx.new(|cx| {
            crate::ui::ai_settings_editor::AiSettingsEditor::new(app, theme, config, kind, cx)
        }));
        self.clear_all_text_input_focus();
        self.placeholder_notice = format!("已打开{}", kind.dialog_title());
    }

    /// 关闭 AI 配置编辑器，不保存尚未提交的草稿。
    pub(crate) fn close_ai_settings_editor(&mut self) {
        self.ai_settings_editor_modal = None;
        self.placeholder_notice = "已关闭智能分析配置对话框".to_string();
    }

    /// 校验并持久化 AI 非敏感配置和可选 API Key。
    pub(crate) fn save_ai_settings(
        &mut self,
        mut config: crate::config::AiConfig,
        credential: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        config.normalize();
        if !config.model_profiles.is_empty() {
            config.validate_model_profiles()?;
        }
        config.validate_log_profiles()?;
        config.validate_system_prompt()?;
        // 设置允许暂时停用全部模型；入口会把它解释为“当前无可选择模型”，而不是依赖已删除的全局开关。
        if config.allow_raw_log_content
            && config.consent_version != crate::config::AI_RAW_LOG_CONSENT_VERSION
        {
            return Err("日志原文授权说明已更新，请在设置中重新确认".to_string());
        }
        if let Some((base_url, api_key)) = credential
            && !api_key.trim().is_empty()
        {
            // 凭据按规范化端点作为系统凭据库账号键；相同端点下的多个模型安全复用密钥。
            let mut credential_base_url = base_url.trim().trim_end_matches('/').to_string();
            if !credential_base_url.is_empty() && !credential_base_url.ends_with("/v1") {
                credential_base_url.push_str("/v1");
            }
            crate::config::ai_config::validate_ai_base_url(&credential_base_url)?;
            crate::agent::save_api_key(&credential_base_url, api_key.trim())?;
        }
        let assistant_scope_configuration_changed = self.config.ai.allow_raw_log_content
            != config.allow_raw_log_content
            || self.config.ai.consent_version != config.consent_version
            || self.config.ai.log_profiles != config.log_profiles;
        let mut next_app_config = self.config.clone();
        next_app_config.ai = config;
        self.config_manager
            .save(&next_app_config)
            .map_err(|error| format!("保存智能分析设置失败：{error}"))?;
        self.config = next_app_config;
        self.ai_settings_editor_modal = None;
        self.placeholder_notice = "智能分析设置已保存".to_string();
        if let Some(panel) = self.assistant_panel.clone() {
            let source_revision = self.source_content_revision;
            let ai_config = self.config.ai.clone();
            panel.update(cx, move |panel, panel_cx| {
                panel.apply_model_configuration(ai_config);
                if assistant_scope_configuration_changed {
                    panel.invalidate_scope_for_analysis_configuration_change(
                        source_revision,
                        "日志类型说明或日志原文授权已经更新",
                        panel_cx,
                    );
                }
                panel_cx.notify();
            });
        }
        Ok(())
    }

    /// 将指定设置文本输入统一写回配置并持久化。
    ///
    /// 键盘编辑、原生输入法提交以及显式清空均通过此入口提交，确保规范化规则、缓存刷新和提示一致。
    pub(crate) fn commit_settings_text_input(&mut self, target: AppTextInputTarget) {
        match target {
            AppTextInputTarget::SettingsQuickKeywords => {
                self.config.log_search.quick_keywords =
                    self.settings_quick_keywords_input.value.clone();
                self.placeholder_notice = "快搜关键字已保存".to_string();
            }
            _ => return,
        }
        self.persist_config_or_report();
    }

    /// 打开主窗口内的设置模态框，并刷新依赖系统能力的设置状态。
    ///
    /// 参数说明：
    /// - `cx`：主应用上下文，用于刷新系统右键菜单注册状态。
    pub(crate) fn open_settings_modal(&mut self, cx: &mut Context<Self>) {
        self.refresh_open_with_registration_status(cx);
        self.is_settings_modal_open = true;
        self.is_theme_dropdown_open = false;
        self.clear_all_text_input_focus();
        // 规则命中徽标依赖打开中的分析结果，进入设置页时同步一次保证数据新鲜。
        self.refresh_jstack_thread_filter();
        self.placeholder_notice = "已打开设置".to_string();
    }

    /// 关闭设置模态框，并清理下拉菜单和输入框临时焦点状态。
    pub(crate) fn close_settings_modal(&mut self) {
        self.is_settings_modal_open = false;
        self.is_theme_dropdown_open = false;
        self.clear_all_text_input_focus();
        self.placeholder_notice = "已关闭设置".to_string();
    }

    /// 切换设置模态框右侧展示的分类内容。
    ///
    /// 参数说明：
    /// - `section`：用户从左侧导航选择的目标分类。
    pub(crate) fn select_settings_section(&mut self, section: SettingsSection) {
        if self.selected_settings_section == section {
            return;
        }

        self.selected_settings_section = section;
        self.is_theme_dropdown_open = false;
        self.clear_all_text_input_focus();
        self.placeholder_notice = format!("已切换到{}设置", section.label());
    }

    /// 打开 Jstack 过滤规则编辑器；`rule_index` 为 `None` 表示新建规则，编辑器已存在时置前已有窗口。
    ///
    /// 参数说明：
    /// - `rule_index`：要编辑的规则在配置列表中的索引；`None` 表示新增。
    /// - `cx`：主应用上下文，用于创建或激活独立编辑窗口。
    pub(crate) fn open_jstack_filter_rule_editor(
        &mut self,
        rule_index: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        if self.is_jstack_filter_rule_editor_open {
            if let Some(window_handle) = self.jstack_filter_rule_editor_handle
                && window_handle
                    .update(cx, |_, window, _| window.activate_window())
                    .is_ok()
            {
                self.placeholder_notice = "过滤规则编辑器已显示到最前".to_string();
                return;
            }

            // 句柄失效通常表示窗口已被系统关闭；清理后重新创建，避免按钮无响应。
            self.is_jstack_filter_rule_editor_open = false;
            self.jstack_filter_rule_editor_handle = None;
            self.jstack_filter_rule_editor_draft = None;
        }

        let draft = match rule_index {
            Some(index) => {
                let Some(rule) = self
                    .config
                    .log_display
                    .jstack_thread_filter_rules
                    .get(index)
                else {
                    self.placeholder_notice = "未找到要编辑的过滤规则".to_string();
                    return;
                };
                JstackFilterRuleDraft {
                    rule_index: Some(index),
                    kind: rule.kind,
                    input: TextInputState::from_value(rule.pattern.clone()),
                    discard_on_close: false,
                }
            }
            None => JstackFilterRuleDraft {
                rule_index: None,
                kind: JstackThreadFilterRuleKind::ThreadName,
                input: TextInputState::default(),
                discard_on_close: false,
            },
        };

        let app_entity = cx.entity();
        let initial_theme = self.theme.clone();
        let initial_snapshot = JstackFilterRuleEditorWindow::snapshot_from_app(self);
        let bounds = Bounds::centered(
            None,
            size(
                px(JSTACK_FILTER_RULE_EDITOR_WIDTH),
                px(JSTACK_FILTER_RULE_EDITOR_HEIGHT),
            ),
            cx,
        );
        let window_options = WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(
                px(JSTACK_FILTER_RULE_EDITOR_MIN_WIDTH),
                px(JSTACK_FILTER_RULE_EDITOR_MIN_HEIGHT),
            )),
            ..Default::default()
        };

        self.jstack_filter_rule_editor_draft = Some(draft);
        self.is_jstack_filter_rule_editor_open = true;
        self.is_theme_dropdown_open = false;
        self.placeholder_notice = "已打开过滤规则编辑器".to_string();

        match cx.open_window(window_options, move |_, cx| {
            cx.new(|cx| {
                JstackFilterRuleEditorWindow::new(app_entity, initial_theme, initial_snapshot, cx)
            })
        }) {
            Ok(window_handle) => {
                self.jstack_filter_rule_editor_handle = Some(window_handle);
            }
            Err(error) => {
                self.is_jstack_filter_rule_editor_open = false;
                self.jstack_filter_rule_editor_handle = None;
                self.jstack_filter_rule_editor_draft = None;
                self.placeholder_notice = format!("打开过滤规则编辑器失败：{error}");
            }
        }
    }

    /// 关闭 Jstack 过滤规则编辑器并提交草稿；关闭按钮和窗口销毁都走该入口。
    ///
    /// 说明：取消标记优先；草稿内容去除首尾空白后为空时丢弃并提示，否则写回配置、
    /// 持久化并刷新过滤结果，保证“关闭窗口即提交”的交互约定。
    pub(crate) fn close_jstack_filter_rule_editor(&mut self) {
        self.is_jstack_filter_rule_editor_open = false;
        self.jstack_filter_rule_editor_handle = None;
        let Some(draft) = self.jstack_filter_rule_editor_draft.take() else {
            return;
        };
        if draft.discard_on_close {
            self.placeholder_notice = "已取消过滤规则编辑".to_string();
            return;
        }

        let pattern = draft.input.value.trim().to_string();
        if pattern.is_empty() {
            self.placeholder_notice = "规则内容为空，未保存".to_string();
            return;
        }

        let rules = &mut self.config.log_display.jstack_thread_filter_rules;
        match draft.rule_index {
            Some(index) if index < rules.len() => {
                rules[index].kind = draft.kind;
                rules[index].pattern = pattern;
            }
            _ => rules.push(JstackThreadFilterRule {
                enabled: true,
                kind: draft.kind,
                pattern,
            }),
        }
        self.persist_config_or_report();
        self.refresh_jstack_thread_filter();
        self.placeholder_notice = "过滤规则已保存".to_string();
    }

    /// 放弃当前规则编辑草稿，并触发与关闭按钮相同的关窗路径。
    pub(crate) fn discard_jstack_filter_rule_editor(&mut self) {
        if let Some(draft) = self.jstack_filter_rule_editor_draft.as_mut() {
            draft.discard_on_close = true;
        }
    }

    /// 切换指定 Jstack 过滤规则的启用状态，并持久化刷新过滤结果。
    pub(crate) fn toggle_jstack_thread_filter_rule(&mut self, index: usize) {
        let Some(rule) = self
            .config
            .log_display
            .jstack_thread_filter_rules
            .get_mut(index)
        else {
            self.placeholder_notice = "未找到要切换的过滤规则".to_string();
            return;
        };
        rule.enabled = !rule.enabled;
        self.persist_config_or_report();
        self.refresh_jstack_thread_filter();
    }

    /// 删除指定 Jstack 过滤规则，并持久化刷新过滤结果。
    pub(crate) fn delete_jstack_thread_filter_rule(&mut self, index: usize) {
        if index >= self.config.log_display.jstack_thread_filter_rules.len() {
            self.placeholder_notice = "未找到要删除的过滤规则".to_string();
            return;
        }
        self.config
            .log_display
            .jstack_thread_filter_rules
            .remove(index);
        self.persist_config_or_report();
        self.refresh_jstack_thread_filter();
        self.placeholder_notice = "过滤规则已删除".to_string();
    }

    /// 直接替换 Jstack 过滤规则列表；测试和未来批量导入入口复用。
    #[cfg(test)]
    pub(super) fn update_jstack_thread_filter_rules(&mut self, rules: Vec<JstackThreadFilterRule>) {
        self.config.log_display.jstack_thread_filter_rules = rules;
        self.persist_config_or_report();
        self.refresh_jstack_thread_filter();
    }

    /// 过滤规则变化时重建全部可见行缓存，并刷新规则命中徽标数据。
    ///
    /// 说明：命中统计是单遍轻量扫描，规则未变化时也执行，保证设置页打开期间
    /// 新完成的分析结果能及时反映到徽标；昂贵的可见行缓存重建仅在配置变化时发生。
    pub(crate) fn refresh_jstack_thread_filter(&mut self) {
        if self.applied_jstack_thread_filter_rules
            != self.config.log_display.jstack_thread_filter_rules
        {
            self.applied_jstack_thread_filter_rules =
                self.config.log_display.jstack_thread_filter_rules.clone();
            self.rebuild_all_jstack_visible_row_caches();
        }
        self.recompute_jstack_filter_rule_hit_counts();
    }

    /// 遍历全部已完成的 Jstack 分析，汇总每条启用规则的命中行数供设置页徽标展示。
    fn recompute_jstack_filter_rule_hit_counts(&mut self) {
        let rules = &self.config.log_display.jstack_thread_filter_rules;
        let filter = JstackThreadFilter::from_rules(rules);
        let mut counts = vec![0_usize; rules.len()];
        for state in self.jstack_analyses.values() {
            let JstackAnalysisTaskState::Ready(result) = &state.task_state else {
                continue;
            };
            for (index, count) in filter
                .rule_hit_counts(&result.rows, rules.len())
                .into_iter()
                .enumerate()
            {
                counts[index] += count;
            }
        }
        self.jstack_filter_rule_hit_counts = counts;
    }

    /// 刷新系统“用 Argus 打开”右键菜单注册状态。
    ///
    /// 说明：状态查询应保持轻量，打开设置模态框和注册/卸载完成后都会调用；忙碌时跳过，
    /// 避免执行中状态被同步查询覆盖。
    pub(crate) fn refresh_open_with_registration_status(&mut self, _cx: &mut Context<Self>) {
        if self.is_open_with_registration_busy {
            return;
        }

        self.open_with_registration_status = registration_status();
    }

    /// 注册系统右键菜单；执行期间禁用注册/卸载按钮，避免重复写入系统状态。
    pub(crate) fn register_open_with_menu(&mut self, cx: &mut Context<Self>) {
        if self.is_open_with_registration_busy {
            self.open_with_registration_message = Some("系统右键菜单操作正在执行".to_string());
            return;
        }

        self.is_open_with_registration_busy = true;
        self.open_with_registration_message = Some("正在注册系统右键菜单...".to_string());
        self.placeholder_notice = "正在注册系统右键菜单".to_string();

        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { register_open_with() })
                .await;

            view.update(cx, |app, cx| {
                app.is_open_with_registration_busy = false;
                match result {
                    Ok(()) => {
                        app.open_with_registration_status = registration_status();
                        app.open_with_registration_message = Some("系统右键菜单已注册".to_string());
                        app.placeholder_notice = "系统右键菜单已注册".to_string();
                    }
                    Err(error) => {
                        app.open_with_registration_status = registration_status();
                        app.open_with_registration_message = Some(error.to_string());
                        app.placeholder_notice = format!("系统右键菜单注册失败：{error}");
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 卸载系统右键菜单；完成后重新查询系统状态并更新设置模态框提示。
    pub(crate) fn unregister_open_with_menu(&mut self, cx: &mut Context<Self>) {
        if self.is_open_with_registration_busy {
            self.open_with_registration_message = Some("系统右键菜单操作正在执行".to_string());
            return;
        }

        self.is_open_with_registration_busy = true;
        self.open_with_registration_message = Some("正在卸载系统右键菜单...".to_string());
        self.placeholder_notice = "正在卸载系统右键菜单".to_string();

        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { unregister_open_with() })
                .await;

            view.update(cx, |app, cx| {
                app.is_open_with_registration_busy = false;
                match result {
                    Ok(()) => {
                        app.open_with_registration_status = registration_status();
                        app.open_with_registration_message = Some("系统右键菜单已卸载".to_string());
                        app.placeholder_notice = "系统右键菜单已卸载".to_string();
                    }
                    Err(error) => {
                        app.open_with_registration_status = registration_status();
                        app.open_with_registration_message = Some(error.to_string());
                        app.placeholder_notice = format!("系统右键菜单卸载失败：{error}");
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 聚焦设置模态框快搜关键字输入框，并关闭设置页的其它浮层。
    pub(crate) fn focus_settings_quick_keywords_input(&mut self) {
        self.is_theme_dropdown_open = false;
        if let Some(draft) = self.jstack_filter_rule_editor_draft.as_mut() {
            draft.input.is_focused = false;
            draft.input.marked_range = None;
        }
        self.settings_quick_keywords_input.is_focused = true;
        self.settings_quick_keywords_input.marked_range = None;
    }

    /// 返回设置模态框快搜关键字输入框当前选区范围。
    ///
    /// 返回值：存在非空选区时返回字符范围；无选区或空选区返回 `None`。
    pub(crate) fn settings_quick_keywords_selection_range(&self) -> Option<Range<usize>> {
        normalized_input_selection_range(&self.settings_quick_keywords_input)
    }

    /// 清空设置模态框快搜关键字输入框，并立即持久化配置。
    pub(crate) fn clear_settings_quick_keywords_input(&mut self) {
        self.settings_quick_keywords_input.value.clear();
        self.settings_quick_keywords_input.cursor = 0;
        self.settings_quick_keywords_input.selection_anchor = None;
        self.settings_quick_keywords_input.marked_range = None;
        self.settings_quick_keywords_input.selection_drag = None;
        self.commit_settings_quick_keywords_input();
    }

    /// 直接更新快搜关键字配置；测试和未来批量设置入口可复用。
    #[cfg(test)]
    pub(super) fn update_settings_quick_keywords(&mut self, value: String) {
        self.settings_quick_keywords_input = TextInputState::from_value(value);
        self.commit_settings_quick_keywords_input();
    }

    /// 处理设置模态框快搜关键字输入框键盘事件。
    ///
    /// 参数说明：
    /// - `keystroke`：GPUI 归一化按键事件。
    /// - `cx`：主应用上下文，用于访问系统剪贴板。
    pub(crate) fn handle_settings_quick_keywords_key(
        &mut self,
        keystroke: &Keystroke,
        cx: &mut Context<Self>,
    ) {
        let key = keystroke.key.as_str();
        let modifiers = keystroke.modifiers;

        if modifiers.platform && key.eq_ignore_ascii_case("a") {
            self.select_all_settings_quick_keywords_input();
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("c") {
            self.copy_settings_quick_keywords_selection(cx);
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("x") {
            self.cut_settings_quick_keywords_selection(cx);
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("v") {
            self.paste_settings_quick_keywords_clipboard(cx);
            return;
        }

        match key {
            "backspace" => self.delete_settings_quick_keywords_backward(),
            "delete" => self.delete_settings_quick_keywords_forward(),
            "left" => self.move_settings_quick_keywords_cursor_left(modifiers.shift),
            "right" => self.move_settings_quick_keywords_cursor_right(modifiers.shift),
            "home" => self.move_settings_quick_keywords_cursor_to(0, modifiers.shift),
            "end" => {
                let text_length = character_count(&self.settings_quick_keywords_input.value);
                self.move_settings_quick_keywords_cursor_to(text_length, modifiers.shift);
            }
            "enter" | "escape" => {
                self.commit_settings_text_input(AppTextInputTarget::SettingsQuickKeywords);
                self.settings_quick_keywords_input.clear_focus();
            }
            _ if key.chars().count() == 1 && !modifiers.control && !modifiers.platform => {
                self.insert_settings_quick_keywords_text(key);
            }
            _ => {}
        }
    }

    /// 开始设置模态框快搜关键字输入框鼠标选择。
    pub(crate) fn begin_settings_quick_keywords_pointer_selection(
        &mut self,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        self.focus_settings_quick_keywords_input();
        self.settings_quick_keywords_input
            .begin_pointer_selection(character_index, granularity);
    }

    /// 更新设置模态框快搜关键字输入框鼠标拖拽选择。
    pub(crate) fn update_settings_quick_keywords_pointer_selection(
        &mut self,
        character_index: usize,
    ) {
        self.settings_quick_keywords_input
            .update_pointer_selection(character_index);
    }

    /// 结束设置模态框快搜关键字输入框鼠标选择。
    pub(crate) fn finish_settings_quick_keywords_pointer_selection(&mut self) {
        self.settings_quick_keywords_input
            .finish_pointer_selection();
    }

    /// 将设置输入框内容写回配置并保存。
    fn commit_settings_quick_keywords_input(&mut self) {
        self.commit_settings_text_input(AppTextInputTarget::SettingsQuickKeywords);
    }

    /// 向设置快搜输入框插入文本。
    fn insert_settings_quick_keywords_text(&mut self, text: &str) {
        self.delete_settings_quick_keywords_selection();
        let input = &mut self.settings_quick_keywords_input;
        input.value = insert_text_at_character_index(&input.value, input.cursor, text);
        input.cursor += character_count(text);
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_quick_keywords_input();
    }

    /// 删除设置快搜输入框当前选区。
    fn delete_settings_quick_keywords_selection(&mut self) -> bool {
        let Some(range) = self.settings_quick_keywords_selection_range() else {
            return false;
        };
        let input = &mut self.settings_quick_keywords_input;
        input.value = remove_character_range(&input.value, range.clone());
        input.cursor = range.start;
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_quick_keywords_input();
        true
    }

    /// 从光标前删除一个字符。
    fn delete_settings_quick_keywords_backward(&mut self) {
        if self.delete_settings_quick_keywords_selection()
            || self.settings_quick_keywords_input.cursor == 0
        {
            return;
        }
        let cursor = self.settings_quick_keywords_input.cursor;
        let input = &mut self.settings_quick_keywords_input;
        input.value = remove_character_range(&input.value, cursor - 1..cursor);
        input.cursor -= 1;
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_quick_keywords_input();
    }

    /// 从光标后删除一个字符。
    fn delete_settings_quick_keywords_forward(&mut self) {
        if self.delete_settings_quick_keywords_selection() {
            return;
        }
        let cursor = self.settings_quick_keywords_input.cursor;
        let text_length = character_count(&self.settings_quick_keywords_input.value);
        if cursor >= text_length {
            return;
        }
        let input = &mut self.settings_quick_keywords_input;
        input.value = remove_character_range(&input.value, cursor..cursor + 1);
        input.marked_range = None;
        input.selection_drag = None;
        self.commit_settings_quick_keywords_input();
    }

    /// 左移设置快搜输入框光标。
    fn move_settings_quick_keywords_cursor_left(&mut self, extend_selection: bool) {
        let cursor = self.settings_quick_keywords_input.cursor.saturating_sub(1);
        self.move_settings_quick_keywords_cursor_to(cursor, extend_selection);
    }

    /// 右移设置快搜输入框光标。
    fn move_settings_quick_keywords_cursor_right(&mut self, extend_selection: bool) {
        let text_length = character_count(&self.settings_quick_keywords_input.value);
        let cursor = (self.settings_quick_keywords_input.cursor + 1).min(text_length);
        self.move_settings_quick_keywords_cursor_to(cursor, extend_selection);
    }

    /// 移动设置快搜输入框光标，并按需扩展选区。
    fn move_settings_quick_keywords_cursor_to(&mut self, cursor: usize, extend_selection: bool) {
        let text_length = character_count(&self.settings_quick_keywords_input.value);
        let cursor = cursor.min(text_length);
        let input = &mut self.settings_quick_keywords_input;
        if extend_selection {
            input.selection_anchor.get_or_insert(input.cursor);
        } else {
            input.selection_anchor = None;
        }
        input.cursor = cursor;
        input.marked_range = None;
        input.selection_drag = None;
    }

    /// 全选设置快搜输入框文本。
    fn select_all_settings_quick_keywords_input(&mut self) {
        self.settings_quick_keywords_input.selection_anchor = Some(0);
        self.settings_quick_keywords_input.cursor =
            character_count(&self.settings_quick_keywords_input.value);
        self.settings_quick_keywords_input.marked_range = None;
        self.settings_quick_keywords_input.selection_drag = None;
    }

    /// 复制设置快搜输入框选中文本。
    fn copy_settings_quick_keywords_selection(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.selected_settings_quick_keywords_text() else {
            return;
        };
        let app_context: &gpui::App = (*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// 剪切设置快搜输入框选中文本。
    fn cut_settings_quick_keywords_selection(&mut self, cx: &mut Context<Self>) {
        self.copy_settings_quick_keywords_selection(cx);
        self.delete_settings_quick_keywords_selection();
    }

    /// 粘贴剪贴板文本到设置快搜输入框。
    fn paste_settings_quick_keywords_clipboard(&mut self, cx: &mut Context<Self>) {
        let app_context: &gpui::App = (*cx).borrow();
        let Some(item) = app_context.read_from_clipboard() else {
            return;
        };
        if let Some(text) = item.text() {
            self.insert_settings_quick_keywords_text(&text.replace(['\n', '\r'], " "));
        }
    }

    /// 返回设置快搜输入框选中文本。
    fn selected_settings_quick_keywords_text(&self) -> Option<String> {
        let range = self.settings_quick_keywords_selection_range()?;
        Some(slice_character_range(
            &self.settings_quick_keywords_input.value,
            range,
        ))
    }

    /// 返回规则编辑器草稿输入框的可变引用；编辑器未打开时返回 `None`。
    fn jstack_filter_rule_editor_input_mut(&mut self) -> Option<&mut TextInputState> {
        self.jstack_filter_rule_editor_draft
            .as_mut()
            .map(|draft| &mut draft.input)
    }

    /// 聚焦 Jstack 过滤规则编辑器内容输入框，并关闭设置页其它输入框焦点。
    pub(crate) fn focus_jstack_filter_rule_editor_input(&mut self) {
        self.is_theme_dropdown_open = false;
        self.settings_quick_keywords_input.is_focused = false;
        self.settings_quick_keywords_input.marked_range = None;
        if let Some(draft) = self.jstack_filter_rule_editor_draft.as_mut() {
            draft.input.is_focused = true;
            draft.input.marked_range = None;
        }
    }

    /// 返回 Jstack 过滤规则编辑器内容输入框当前选区范围。
    pub(crate) fn jstack_filter_rule_editor_selection_range(&self) -> Option<Range<usize>> {
        normalized_input_selection_range(&self.jstack_filter_rule_editor_draft.as_ref()?.input)
    }

    /// 清空 Jstack 过滤规则编辑器内容输入框；只修改草稿，不产生任何持久化副作用。
    pub(crate) fn clear_jstack_filter_rule_editor_input(&mut self) {
        let Some(input) = self.jstack_filter_rule_editor_input_mut() else {
            return;
        };
        input.value.clear();
        input.cursor = 0;
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
    }

    /// 更新 Jstack 过滤规则编辑器的草稿匹配方式；只修改草稿，不影响已保存配置。
    pub(crate) fn update_jstack_filter_rule_editor_kind(
        &mut self,
        kind: JstackThreadFilterRuleKind,
    ) {
        if let Some(draft) = self.jstack_filter_rule_editor_draft.as_mut() {
            draft.kind = kind;
        }
    }

    /// 处理 Jstack 过滤规则编辑器内容输入框键盘事件。
    ///
    /// 说明：编辑只写入草稿，提交统一由关闭窗口路径完成，按键期间不触发任何配置写盘。
    ///
    /// 参数说明：
    /// - `keystroke`：GPUI 归一化按键事件。
    /// - `cx`：主应用上下文，用于访问系统剪贴板。
    pub(crate) fn handle_jstack_filter_rule_editor_key(
        &mut self,
        keystroke: &Keystroke,
        cx: &mut Context<Self>,
    ) {
        let key = keystroke.key.as_str();
        let modifiers = keystroke.modifiers;

        if modifiers.platform && key.eq_ignore_ascii_case("a") {
            self.select_all_jstack_filter_rule_editor_input();
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("c") {
            self.copy_jstack_filter_rule_editor_selection(cx);
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("x") {
            self.cut_jstack_filter_rule_editor_selection(cx);
            return;
        }
        if modifiers.platform && key.eq_ignore_ascii_case("v") {
            self.paste_jstack_filter_rule_editor_clipboard(cx);
            return;
        }

        match key {
            "backspace" => self.delete_jstack_filter_rule_editor_backward(),
            "delete" => self.delete_jstack_filter_rule_editor_forward(),
            "enter" => self.insert_jstack_filter_rule_editor_text("\n"),
            "left" => self.move_jstack_filter_rule_editor_cursor_left(modifiers.shift),
            "right" => self.move_jstack_filter_rule_editor_cursor_right(modifiers.shift),
            "up" => self.move_jstack_filter_rule_editor_cursor_vertically(-1, modifiers.shift),
            "down" => self.move_jstack_filter_rule_editor_cursor_vertically(1, modifiers.shift),
            "home" => {
                let Some(input) = self.jstack_filter_rule_editor_input_mut() else {
                    return;
                };
                let cursor = current_line_range(&input.value, input.cursor).start;
                self.move_jstack_filter_rule_editor_cursor_to(cursor, modifiers.shift);
            }
            "end" => {
                let Some(input) = self.jstack_filter_rule_editor_input_mut() else {
                    return;
                };
                let cursor = current_line_range(&input.value, input.cursor).end;
                self.move_jstack_filter_rule_editor_cursor_to(cursor, modifiers.shift);
            }
            _ if key.chars().count() == 1
                && !modifiers.control
                && !modifiers.platform
                && !key.chars().any(char::is_control) =>
            {
                self.insert_jstack_filter_rule_editor_text(key);
            }
            _ => {}
        }
    }

    /// 开始 Jstack 过滤规则编辑器内容输入框鼠标选择。
    pub(crate) fn begin_jstack_filter_rule_editor_pointer_selection(
        &mut self,
        character_index: usize,
        granularity: TextSelectionGranularity,
    ) {
        self.focus_jstack_filter_rule_editor_input();
        if let Some(draft) = self.jstack_filter_rule_editor_draft.as_mut() {
            draft
                .input
                .begin_pointer_selection(character_index, granularity);
        }
    }

    /// 更新 Jstack 过滤规则编辑器内容输入框鼠标拖拽选择。
    pub(crate) fn update_jstack_filter_rule_editor_pointer_selection(
        &mut self,
        character_index: usize,
    ) {
        if let Some(draft) = self.jstack_filter_rule_editor_draft.as_mut() {
            draft.input.update_pointer_selection(character_index);
        }
    }

    /// 结束 Jstack 过滤规则编辑器内容输入框鼠标选择。
    pub(crate) fn finish_jstack_filter_rule_editor_pointer_selection(&mut self) {
        if let Some(draft) = self.jstack_filter_rule_editor_draft.as_mut() {
            draft.input.finish_pointer_selection();
        }
    }

    /// 向 Jstack 过滤规则编辑器输入框插入文本；只修改草稿，不触发配置提交。
    fn insert_jstack_filter_rule_editor_text(&mut self, text: &str) {
        self.delete_jstack_filter_rule_editor_selection();
        let text = normalized_textarea_value(text);
        let Some(input) = self.jstack_filter_rule_editor_input_mut() else {
            return;
        };
        input.value = insert_text_at_character_index(&input.value, input.cursor, &text);
        input.cursor += character_count(&text);
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
    }

    /// 删除 Jstack 过滤规则编辑器输入框当前选区。
    fn delete_jstack_filter_rule_editor_selection(&mut self) -> bool {
        let Some(range) = self.jstack_filter_rule_editor_selection_range() else {
            return false;
        };
        let Some(input) = self.jstack_filter_rule_editor_input_mut() else {
            return false;
        };
        input.value = remove_character_range(&input.value, range.clone());
        input.cursor = range.start;
        input.selection_anchor = None;
        input.marked_range = None;
        input.selection_drag = None;
        true
    }

    /// 从 Jstack 过滤规则编辑器输入框光标前删除一个字符。
    fn delete_jstack_filter_rule_editor_backward(&mut self) {
        if self.delete_jstack_filter_rule_editor_selection() {
            return;
        }
        let Some(input) = self.jstack_filter_rule_editor_input_mut() else {
            return;
        };
        if input.cursor == 0 {
            return;
        }
        let cursor = input.cursor;
        input.value = remove_character_range(&input.value, cursor - 1..cursor);
        input.cursor -= 1;
        input.marked_range = None;
        input.selection_drag = None;
    }

    /// 从 Jstack 过滤规则编辑器输入框光标后删除一个字符。
    fn delete_jstack_filter_rule_editor_forward(&mut self) {
        if self.delete_jstack_filter_rule_editor_selection() {
            return;
        }
        let Some(input) = self.jstack_filter_rule_editor_input_mut() else {
            return;
        };
        let cursor = input.cursor;
        let text_length = character_count(&input.value);
        if cursor >= text_length {
            return;
        }
        input.value = remove_character_range(&input.value, cursor..cursor + 1);
        input.marked_range = None;
        input.selection_drag = None;
    }

    /// 左移 Jstack 过滤规则编辑器输入框光标。
    fn move_jstack_filter_rule_editor_cursor_left(&mut self, extend_selection: bool) {
        let cursor = self
            .jstack_filter_rule_editor_draft
            .as_ref()
            .map_or(0, |draft| draft.input.cursor.saturating_sub(1));
        self.move_jstack_filter_rule_editor_cursor_to(cursor, extend_selection);
    }

    /// 右移 Jstack 过滤规则编辑器输入框光标。
    fn move_jstack_filter_rule_editor_cursor_right(&mut self, extend_selection: bool) {
        let cursor = self
            .jstack_filter_rule_editor_draft
            .as_ref()
            .map_or(0, |draft| {
                let text_length = character_count(&draft.input.value);
                (draft.input.cursor + 1).min(text_length)
            });
        self.move_jstack_filter_rule_editor_cursor_to(cursor, extend_selection);
    }

    /// 上下移动 Jstack 过滤规则编辑器输入框光标，尽量保持当前列位置。
    fn move_jstack_filter_rule_editor_cursor_vertically(
        &mut self,
        direction: isize,
        extend_selection: bool,
    ) {
        let next_cursor = self
            .jstack_filter_rule_editor_draft
            .as_ref()
            .map_or(0, |draft| {
                vertical_cursor_position(&draft.input.value, draft.input.cursor, direction)
            });
        self.move_jstack_filter_rule_editor_cursor_to(next_cursor, extend_selection);
    }

    /// 移动 Jstack 过滤规则编辑器输入框光标，并按需扩展选区。
    fn move_jstack_filter_rule_editor_cursor_to(&mut self, cursor: usize, extend_selection: bool) {
        let Some(input) = self.jstack_filter_rule_editor_input_mut() else {
            return;
        };
        let cursor = cursor.min(character_count(&input.value));
        if extend_selection {
            input.selection_anchor.get_or_insert(input.cursor);
        } else {
            input.selection_anchor = None;
        }
        input.cursor = cursor;
        input.marked_range = None;
        input.selection_drag = None;
    }

    /// 全选 Jstack 过滤规则编辑器输入框文本。
    fn select_all_jstack_filter_rule_editor_input(&mut self) {
        let Some(input) = self.jstack_filter_rule_editor_input_mut() else {
            return;
        };
        input.selection_anchor = Some(0);
        input.cursor = character_count(&input.value);
        input.marked_range = None;
        input.selection_drag = None;
    }

    /// 复制 Jstack 过滤规则编辑器输入框选中文本。
    fn copy_jstack_filter_rule_editor_selection(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.selected_jstack_filter_rule_editor_text() else {
            return;
        };
        let app_context: &gpui::App = (*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// 剪切 Jstack 过滤规则编辑器输入框选中文本。
    fn cut_jstack_filter_rule_editor_selection(&mut self, cx: &mut Context<Self>) {
        self.copy_jstack_filter_rule_editor_selection(cx);
        self.delete_jstack_filter_rule_editor_selection();
    }

    /// 粘贴剪贴板文本到 Jstack 过滤规则编辑器输入框；保留真实换行以匹配完整线程段。
    fn paste_jstack_filter_rule_editor_clipboard(&mut self, cx: &mut Context<Self>) {
        let app_context: &gpui::App = (*cx).borrow();
        let Some(item) = app_context.read_from_clipboard() else {
            return;
        };
        if let Some(text) = item.text() {
            self.insert_jstack_filter_rule_editor_text(&text);
        }
    }

    /// 返回 Jstack 过滤规则编辑器输入框选中文本。
    fn selected_jstack_filter_rule_editor_text(&self) -> Option<String> {
        let range = self.jstack_filter_rule_editor_selection_range()?;
        Some(slice_character_range(
            &self.jstack_filter_rule_editor_draft.as_ref()?.input.value,
            range,
        ))
    }
}

/// 返回输入状态中的规范化非空选区。
fn normalized_input_selection_range(input: &TextInputState) -> Option<Range<usize>> {
    input.selection_range()
}

/// 归一化 textarea 文本，统一系统换行符但保留真实多行结构。
fn normalized_textarea_value(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

/// 返回光标所在行的字符范围，不包含行尾换行符。
fn current_line_range(value: &str, cursor: usize) -> Range<usize> {
    let chars = value.chars().collect::<Vec<_>>();
    let text_length = chars.len();
    let cursor = cursor.min(text_length);
    let mut start = cursor;
    while start > 0 && chars[start - 1] != '\n' {
        start -= 1;
    }
    let mut end = cursor;
    while end < text_length && chars[end] != '\n' {
        end += 1;
    }
    start..end
}

/// 上下移动多行文本光标，并尽量保持原始列位置。
fn vertical_cursor_position(value: &str, cursor: usize, direction: isize) -> usize {
    let chars = value.chars().collect::<Vec<_>>();
    let text_length = chars.len();
    let cursor = cursor.min(text_length);
    let current_line = current_line_range(value, cursor);
    let current_column = cursor.saturating_sub(current_line.start);

    if direction < 0 {
        if current_line.start == 0 {
            return cursor;
        }
        let previous_line_end = current_line.start - 1;
        let previous_line = current_line_range(value, previous_line_end);
        return previous_line.start
            + current_column.min(previous_line.end.saturating_sub(previous_line.start));
    }

    if current_line.end >= text_length {
        return cursor;
    }
    let next_line_start = current_line.end + 1;
    let next_line = current_line_range(value, next_line_start);
    next_line.start + current_column.min(next_line.end.saturating_sub(next_line.start))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 textarea 行范围只覆盖当前行，不会像单行输入框那样全选。
    #[test]
    fn textarea_line_granularity_selects_current_line() {
        let input = TextInputState::from_value("first\nsecond\nthird".to_string());

        let range = input.range_for_granularity(8, TextSelectionGranularity::Line);

        assert_eq!(slice_character_range(&input.value, range), "second");
    }

    /// 验证 textarea 上下移动光标时尽量保持列位置，并在短行处夹到行尾。
    #[test]
    fn textarea_vertical_cursor_keeps_column_when_possible() {
        let value = "abcdef\nxy\n123456";

        assert_eq!(vertical_cursor_position(value, 4, 1), 9);
        assert_eq!(vertical_cursor_position(value, 9, 1), 12);
        assert_eq!(vertical_cursor_position(value, 12, -1), 9);
    }

    /// 验证 textarea 文本归一化会统一换行符但保留多行结构。
    #[test]
    fn textarea_normalization_preserves_real_newlines() {
        assert_eq!(normalized_textarea_value("a\r\nb\rc"), "a\nb\nc");
    }
}
