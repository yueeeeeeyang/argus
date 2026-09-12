//! 文件职责：渲染主窗口中的 AI 日志分析问题输入模态框。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：展示当前来源根、原文授权提示，接收多行问题并在校验成功后创建独立 Agent 窗口。

use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, FocusHandle, FontWeight, IntoElement,
    KeyDownEvent, Render, ScrollHandle, Subscription, Window, div, prelude::*, px, rgb,
};

use crate::app::{ArgusApp, TextInputState};
use crate::config::AiModelProfile;
use crate::fonts::ARGUS_UI_FONT_FAMILY;
use crate::theme::AppTheme;
use crate::ui::components::icon::{ArgusIcon, render_icon};
use crate::ui::components::icon_button::{IconButtonSize, render_icon_button};
use crate::ui::components::input::{
    InputPointerAction, InputPointerEvent, NativeInput, Textarea, TextareaAccessoryPosition,
    TextareaScrollState, TextareaStyle, render_textarea,
};
use crate::ui::components::input_behavior::{LocalInputAction, handle_local_input_key};
use crate::ui::components::loading_spinner::render_loading_spinner;
use crate::ui::components::modal_dialog::{ModalDialog, render_modal_dialog};

/// 启动问题模态框宽度。
const AGENT_LAUNCH_DIALOG_WIDTH: f32 = 620.0;
/// 启动问题模态框高度。
const AGENT_LAUNCH_DIALOG_HEIGHT: f32 = 430.0;
/// 单次初始问题 UTF-8 字节上限。
const AGENT_QUESTION_MAX_BYTES: usize = 4 * 1024;

/// AI 问题输入模态框子视图，输入状态与主应用其它输入隔离。
pub(crate) struct AgentLaunchDialog {
    /// 主应用实体，用于启动会话和关闭模态框。
    app: Entity<ArgusApp>,
    /// 当前主题快照。
    theme: AppTheme,
    /// 预计分析范围展示名称。
    scope_label: String,
    /// 本次可选择的已启用模型配置快照。
    models: Vec<AiModelProfile>,
    /// 当前选择的模型列表索引。
    selected_model: usize,
    /// 入口预检失败原因；存在时对话框仅展示不可用状态。
    unavailable_reason: Option<String>,
    /// 当前配置是否允许发送必要日志原文。
    allow_raw_log_content: bool,
    /// 用户问题输入状态。
    question_input: TextInputState,
    /// 问题文本域滚动句柄。
    question_scroll: ScrollHandle,
    /// 文本域自绘滚动交互状态。
    question_scroll_state: TextareaScrollState,
    /// 模态框根焦点。
    root_focus: FocusHandle,
    /// 问题文本域原生输入焦点。
    question_focus: FocusHandle,
    /// 最近一次校验或窗口创建错误。
    error_message: Option<String>,
    /// 是否正在后台完整扫描来源树并匹配日志类型。
    is_preparing: bool,
    /// 首次渲染是否已聚焦问题文本域。
    has_focused: bool,
    /// 主应用主题订阅。
    _app_observer: Subscription,
}

impl AgentLaunchDialog {
    /// 创建问题输入模态框并订阅主题变化。
    pub(crate) fn new(
        app: Entity<ArgusApp>,
        theme: AppTheme,
        scope_label: String,
        models: Vec<AiModelProfile>,
        unavailable_reason: Option<String>,
        allow_raw_log_content: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let _app_observer = cx.observe(&app, |dialog, app_entity, cx| {
            let next_theme = app_entity.read_with(cx, |app, _| app.theme.clone());
            if dialog.theme != next_theme {
                dialog.theme = next_theme;
                cx.notify();
            }
        });
        let question_input = TextInputState {
            is_focused: true,
            ..TextInputState::default()
        };
        Self {
            app,
            theme,
            scope_label,
            models,
            selected_model: 0,
            unavailable_reason,
            allow_raw_log_content,
            question_input,
            question_scroll: ScrollHandle::new(),
            question_scroll_state: TextareaScrollState::new(),
            root_focus: cx.focus_handle(),
            question_focus: cx.focus_handle(),
            error_message: None,
            is_preparing: false,
            has_focused: false,
            _app_observer,
        }
    }

    /// 校验问题并请求主应用创建独立窗口和后台会话。
    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.unavailable_reason.is_some() || self.is_preparing {
            return;
        }
        let question = self.question_input.value.trim().to_string();
        if question.is_empty() {
            self.error_message = Some("请输入希望 Agent 分析的问题".to_string());
            return;
        }
        if question.len() > AGENT_QUESTION_MAX_BYTES {
            self.error_message = Some("问题内容不能超过 4 KiB".to_string());
            return;
        }
        let Some(model) = self.models.get(self.selected_model) else {
            self.error_message = Some("当前没有可用于智能分析的模型".to_string());
            return;
        };
        let model_profile_id = model.profile_id.clone();
        self.is_preparing = true;
        self.error_message = None;
        let result = self.app.update(cx, |app, app_cx| {
            app.start_ai_agent_session(question, model_profile_id, app_cx)
        });
        if let Err(message) = result {
            self.is_preparing = false;
            self.error_message = Some(message);
        }
    }

    /// 后台来源扫描失败时恢复表单并显示可重试错误。
    pub(crate) fn finish_preparing_with_error(&mut self, message: String) {
        self.is_preparing = false;
        self.error_message = Some(message);
    }

    /// 在多个已启用模型之间循环选择；单模型时保持当前选择不变。
    fn select_next_model(&mut self) {
        if !self.is_preparing && self.models.len() > 1 {
            self.selected_model = (self.selected_model + 1) % self.models.len();
            self.error_message = None;
        }
    }

    /// 处理问题文本域按键并在 Cmd/Ctrl+Enter 时提交。
    fn handle_question_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.is_preparing {
            return;
        }
        match handle_local_input_key(&mut self.question_input, &event.keystroke, true, cx) {
            LocalInputAction::Submit => self.submit(cx),
            LocalInputAction::Close => {
                self.app.update(cx, |app, app_cx| {
                    app.close_ai_agent_launch_dialog();
                    app_cx.notify();
                });
            }
            LocalInputAction::Changed => self.error_message = None,
            LocalInputAction::None => {}
        }
    }
}

impl Render for AgentLaunchDialog {
    /// 渲染问题、范围和安全提示。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.has_focused {
            if self.unavailable_reason.is_some() {
                self.root_focus.focus(window);
            } else {
                self.question_focus.focus(window);
            }
            self.has_focused = true;
        }
        let entity = cx.entity();
        let native_entity = entity.clone();
        let native_input = NativeInput::new(self.question_focus.clone(), move |edit, _, app_cx| {
            native_entity.update(app_cx, |dialog, cx| {
                if dialog.is_preparing {
                    return;
                }
                dialog.question_input.apply_native_edit(&edit);
                dialog.error_message = None;
                cx.notify();
            });
        });
        let key_entity = entity.clone();
        let click_entity = entity.clone();
        let pointer_entity = entity.clone();
        let submit_entity = entity.clone();
        let select_model_entity = entity.clone();
        let escape_app = self.app.clone();
        let header_close_app = self.app.clone();
        let footer_close_app = self.app.clone();
        let open_settings_app = self.app.clone();
        let theme = self.theme.clone();
        let is_preparing = self.is_preparing;
        let selected_model = self.models.get(self.selected_model).cloned();
        let available_body = div()
            .flex_1()
            .min_h(px(0.0))
            .p_5()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(theme.foreground_muted))
                    .child(format!(
                        "分析范围：{}（开始后会完整扫描该来源根，不依赖树节点展开状态）",
                        self.scope_label
                    )),
            )
            .when_some(selected_model, |this, model| {
                let can_switch = !is_preparing && self.models.len() > 1;
                this.child(
                    div()
                        .id("agent-launch-model-selector")
                        .min_h(px(48.0))
                        .px_3()
                        .py_2()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .rounded_sm()
                        .border_1()
                        .border_color(rgb(theme.border))
                        .bg(rgb(theme.current_line))
                        .when(can_switch, |this| {
                            this.cursor_pointer()
                                .hover(|this| this.bg(rgb(theme.selection)))
                        })
                        .child(
                            div()
                                .min_w(px(0.0))
                                .flex_1()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(format!("使用模型：{}", model.name)),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(10.0))
                                        .text_color(rgb(theme.foreground_muted))
                                        .child(format!(
                                            "{} · 上下文 {} · {}",
                                            model.model,
                                            model.context_window_label(),
                                            model.base_url
                                        )),
                                ),
                        )
                        .when(can_switch, |this| {
                            this.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_size(px(10.0))
                                    .text_color(rgb(theme.foreground_muted))
                                    .child(render_icon(
                                        ArgusIcon::Refresh,
                                        theme.foreground_muted,
                                        12.0,
                                    ))
                                    .child("点击切换"),
                            )
                        })
                        .on_click(move |_, _, app_cx| {
                            app_cx.stop_propagation();
                            select_model_entity.update(app_cx, |dialog, cx| {
                                dialog.select_next_model();
                                cx.notify();
                            });
                        }),
                )
            })
            .child(render_textarea(
                Textarea {
                    id: "agent-launch-question-input",
                    placeholder: "例如：最近一次启动失败的根因是什么？请给出可复核证据。",
                    value: self.question_input.value.clone(),
                    is_disabled: is_preparing,
                    is_focused: self.question_input.is_focused,
                    cursor_index: self.question_input.cursor,
                    selection_range: self.question_input.selection_range(),
                    marked_range: self.question_input.marked_range.clone(),
                    is_pointer_selecting: self.question_input.selection_drag.is_some(),
                    visible_lines: 7,
                    fill_height: false,
                    scroll_handle: self.question_scroll.clone(),
                    scroll_state: self.question_scroll_state.clone(),
                    style: TextareaStyle::Default,
                    trailing_accessory: None,
                    trailing_accessory_position: TextareaAccessoryPosition::TopRight,
                    trailing_accessory_always_visible: false,
                    trailing_accessory_selected: false,
                    native_input: Some(native_input),
                },
                &theme,
                move |event, _, app_cx| {
                    app_cx.stop_propagation();
                    key_entity.update(app_cx, |dialog, cx| {
                        dialog.handle_question_key(event, cx);
                        cx.notify();
                    });
                },
                move |_, window, app_cx| {
                    app_cx.stop_propagation();
                    click_entity.update(app_cx, |dialog, cx| {
                        dialog.question_input.is_focused = true;
                        dialog.question_focus.focus(window);
                        cx.notify();
                    });
                },
                move |event: &InputPointerEvent, _, app_cx| {
                    app_cx.stop_propagation();
                    pointer_entity.update(app_cx, |dialog, cx| {
                        match event.action {
                            InputPointerAction::Begin => dialog
                                .question_input
                                .begin_pointer_selection(event.character_index, event.granularity),
                            InputPointerAction::Extend => dialog
                                .question_input
                                .update_pointer_selection(event.character_index),
                            InputPointerAction::Finish => {
                                dialog.question_input.finish_pointer_selection()
                            }
                        }
                        cx.notify();
                    });
                },
                |_, _, _| {},
            ))
            .when(!is_preparing, |this| {
                this.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(if self.allow_raw_log_content {
                            theme.warning
                        } else {
                            theme.foreground_muted
                        }))
                        .child(if self.allow_raw_log_content {
                            "已授权：Agent 可把工具裁剪并脱敏的必要日志片段发送到所选模型服务。"
                        } else {
                            "未授权日志原文：Agent 只能使用来源元数据和本地聚合结果。"
                        }),
                )
            })
            .when(is_preparing, |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(11.0))
                        .text_color(rgb(theme.info))
                        .child(render_loading_spinner(
                            ("agent-source-scan-loading", 0),
                            theme.info,
                            13.0,
                        ))
                        .child("正在完整扫描来源树并匹配日志类型，完成后自动启动分析…"),
                )
            })
            .when_some(self.error_message.clone(), |this, message| {
                this.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(theme.error))
                        .child(message),
                )
            });

        let unavailable_body = div()
            .flex_1()
            .min_h(px(0.0))
            .p_5()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_4()
            .child(render_icon(ArgusIcon::Info, theme.warning, 32.0))
            .child(
                div()
                    .text_size(px(14.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("当前无法使用智能分析"),
            )
            .child(
                div()
                    .max_w(px(480.0))
                    .text_center()
                    .text_size(px(12.0))
                    .line_height(px(20.0))
                    .text_color(rgb(theme.foreground_muted))
                    .child(
                        self.unavailable_reason
                            .clone()
                            .unwrap_or_else(|| "智能分析配置不可用".to_string()),
                    ),
            )
            .child(dialog_button(
                "agent-launch-open-settings",
                "打开模型配置",
                true,
                true,
                &theme,
                move |_, _, app_cx| {
                    app_cx.stop_propagation();
                    open_settings_app.update(app_cx, |app, cx| {
                        app.close_ai_agent_launch_dialog();
                        app.select_settings_section(crate::app::SettingsSection::AiModel);
                        app.open_settings_modal(cx);
                        cx.notify();
                    });
                },
            ));

        div()
            .id("agent-launch-dialog-content")
            .size_full()
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
                    escape_app.update(app_cx, |app, cx| {
                        app.close_ai_agent_launch_dialog();
                        cx.notify();
                    });
                }
            })
            .child(
                div()
                    .h(px(58.0))
                    .px_5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(render_icon(ArgusIcon::SmartAnalysis, theme.info, 18.0))
                            .child(
                                div()
                                    .text_size(px(15.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("智能日志分析"),
                            ),
                    )
                    .child(render_icon_button(
                        "agent-launch-close",
                        ArgusIcon::Close,
                        "关闭",
                        false,
                        IconButtonSize::Small,
                        &theme,
                        move |_, _, app_cx| {
                            app_cx.stop_propagation();
                            header_close_app.update(app_cx, |app, cx| {
                                app.close_ai_agent_launch_dialog();
                                cx.notify();
                            });
                        },
                    )),
            )
            .child(if self.unavailable_reason.is_some() {
                unavailable_body.into_any_element()
            } else {
                available_body.into_any_element()
            })
            .when(self.unavailable_reason.is_none(), |this| {
                this.child(
                    div()
                        .h(px(58.0))
                        .px_5()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .child(dialog_button(
                            "agent-launch-cancel",
                            "取消",
                            false,
                            true,
                            &theme,
                            move |_, _, app_cx| {
                                app_cx.stop_propagation();
                                footer_close_app.update(app_cx, |app, cx| {
                                    app.close_ai_agent_launch_dialog();
                                    cx.notify();
                                });
                            },
                        ))
                        .child(dialog_button(
                            "agent-launch-submit",
                            if is_preparing {
                                "正在扫描…"
                            } else {
                                "开始分析"
                            },
                            true,
                            !is_preparing,
                            &theme,
                            move |_, _, app_cx| {
                                app_cx.stop_propagation();
                                submit_entity.update(app_cx, |dialog, cx| {
                                    dialog.submit(cx);
                                    cx.notify();
                                });
                            },
                        )),
                )
            })
    }
}

/// 将 Agent 启动子视图包裹为主窗口模态框。
pub(crate) fn render_agent_launch_modal(
    dialog: Entity<AgentLaunchDialog>,
    theme: &AppTheme,
    cx: &mut Context<ArgusApp>,
) -> AnyElement {
    render_modal_dialog(
        ModalDialog {
            overlay_id: "agent-launch-modal-overlay",
            container_id: "agent-launch-modal-container",
            width: AGENT_LAUNCH_DIALOG_WIDTH,
            height: AGENT_LAUNCH_DIALOG_HEIGHT,
            content: dialog.into_any_element(),
        },
        theme.clone(),
        cx,
    )
    .into_any_element()
}

/// 渲染启动模态框底部按钮。
fn dialog_button(
    id: &'static str,
    label: &'static str,
    primary: bool,
    enabled: bool,
    theme: &AppTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .h(px(32.0))
        .px_4()
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
        .opacity(if enabled { 1.0 } else { 0.45 })
        .text_size(px(12.0))
        .when(primary, |this| this.font_weight(FontWeight::SEMIBOLD))
        .child(label)
        .when(enabled, |this| {
            this.hover(|this| this.opacity(0.82))
                .cursor_pointer()
                .on_click(on_click)
        })
}
