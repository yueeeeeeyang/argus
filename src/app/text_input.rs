//! 文件职责：承接系统原生文本输入提交并写回 Argus 自绘输入框状态。
//! 创建日期：2026-06-16
//! 修改日期：2026-06-18
//! 作者：Argus 开发团队
//! 主要功能：把输入法 UTF-16 编辑结果转换后的字符范围应用到来源搜索、日志搜索、来源选择器和设置输入框。

use std::ops::Range;

use crate::app::{
    AppTextInputTarget, ArgusApp, InputTextSelectionDrag, LogSearchInputKind,
    RuntimeFilterInputKind,
};
use crate::text_selection::{NativeTextEdit, character_count, replace_character_range};

/// 可被原生输入法编辑的单行输入框字段引用。
struct NativeInputParts<'a> {
    /// 当前文本。
    value: &'a mut String,
    /// 当前光标字符位置。
    cursor: &'a mut usize,
    /// 当前选区锚点。
    selection_anchor: &'a mut Option<usize>,
    /// 当前输入法 marked text 范围。
    marked_range: &'a mut Option<Range<usize>>,
    /// 当前鼠标拖拽选区状态。
    selection_drag: &'a mut Option<InputTextSelectionDrag>,
}

impl ArgusApp {
    /// 清除所有自绘输入框的业务焦点和输入法临时态。
    ///
    /// 说明：点击输入框以外区域时调用；只清理焦点、选区和 marked text，
    /// 不修改用户已经输入的文本内容，避免误清配置或搜索条件。
    pub fn clear_all_text_input_focus(&mut self) {
        self.is_source_tree_search_focused = false;
        self.source_tree_search_selection_anchor = None;
        self.source_tree_search_marked_range = None;
        self.source_tree_search_selection_drag = None;

        self.source_picker.is_path_input_focused = false;
        self.source_picker.path_input_selection_anchor = None;
        self.source_picker.path_input_marked_range = None;
        self.source_picker.path_input_selection_drag = None;

        clear_settings_input_focus(&mut self.settings_quick_keywords_input);
        clear_settings_input_focus(&mut self.settings_jstack_thread_name_filter_input);
        clear_settings_input_focus(&mut self.settings_jstack_stack_segment_filter_input);
        clear_settings_input_focus(&mut self.settings_upgrade_server_input);
        clear_settings_input_focus(&mut self.settings_upgrade_public_key_input);

        self.log_search.keyword_input.is_focused = false;
        self.log_search.keyword_input.selection_anchor = None;
        self.log_search.keyword_input.marked_range = None;
        self.log_search.keyword_input.selection_drag = None;
        self.log_search.directory_input.is_focused = false;
        self.log_search.directory_input.selection_anchor = None;
        self.log_search.directory_input.marked_range = None;
        self.log_search.directory_input.selection_drag = None;

        for state in self.runtime_analyses.values_mut() {
            clear_settings_input_focus(&mut state.filter_keyword_input);
            clear_settings_input_focus(&mut state.filter_username_input);
            clear_settings_input_focus(&mut state.filter_start_time_input);
            clear_settings_input_focus(&mut state.filter_end_time_input);
            state.open_time_picker = None;
        }
    }

    /// 应用系统原生文本输入编辑结果。
    ///
    /// 参数说明：
    /// - `target`：被编辑的业务输入框。
    /// - `edit`：输入组件已转换为字符索引的编辑结果。
    pub fn apply_native_text_input_edit(
        &mut self,
        target: AppTextInputTarget,
        edit: NativeTextEdit,
    ) {
        self.focus_native_text_input_target(target);

        match target {
            AppTextInputTarget::SourceTreeSearch => {
                apply_native_edit_to_parts(
                    NativeInputParts {
                        value: &mut self.source_tree_search_query,
                        cursor: &mut self.source_tree_search_cursor,
                        selection_anchor: &mut self.source_tree_search_selection_anchor,
                        marked_range: &mut self.source_tree_search_marked_range,
                        selection_drag: &mut self.source_tree_search_selection_drag,
                    },
                    &edit,
                );
                self.rebuild_filtered_source_ids();
                self.placeholder_notice = if self.source_tree_search_query.is_empty() {
                    "来源树搜索框为空，显示完整目录树".to_string()
                } else {
                    format!(
                        "来源树搜索「{}」命中 {} 个可见节点",
                        self.source_tree_search_query,
                        self.filtered_source_ids.len()
                    )
                };
            }
            AppTextInputTarget::SourcePickerPath => {
                apply_native_edit_to_parts(
                    NativeInputParts {
                        value: &mut self.source_picker.path_input,
                        cursor: &mut self.source_picker.path_input_cursor,
                        selection_anchor: &mut self.source_picker.path_input_selection_anchor,
                        marked_range: &mut self.source_picker.path_input_marked_range,
                        selection_drag: &mut self.source_picker.path_input_selection_drag,
                    },
                    &edit,
                );
                self.source_picker.error_message = None;
            }
            AppTextInputTarget::LogSearch(input_kind) => {
                apply_native_log_search_edit(self, input_kind, &edit);
            }
            AppTextInputTarget::RuntimeFilter {
                analysis_id,
                input_kind,
            } => {
                apply_native_runtime_filter_edit(self, analysis_id, input_kind, &edit);
            }
            AppTextInputTarget::SettingsQuickKeywords => {
                apply_native_edit_to_parts(
                    NativeInputParts {
                        value: &mut self.settings_quick_keywords_input.value,
                        cursor: &mut self.settings_quick_keywords_input.cursor,
                        selection_anchor: &mut self.settings_quick_keywords_input.selection_anchor,
                        marked_range: &mut self.settings_quick_keywords_input.marked_range,
                        selection_drag: &mut self.settings_quick_keywords_input.selection_drag,
                    },
                    &edit,
                );
                if edit.marked_range.is_none() {
                    self.config.log_search.quick_keywords =
                        self.settings_quick_keywords_input.value.clone();
                    self.placeholder_notice = "快搜关键字已保存".to_string();
                    self.persist_config_or_report();
                }
            }
            AppTextInputTarget::SettingsJstackThreadNameFilter => {
                apply_native_edit_to_parts(
                    NativeInputParts {
                        value: &mut self.settings_jstack_thread_name_filter_input.value,
                        cursor: &mut self.settings_jstack_thread_name_filter_input.cursor,
                        selection_anchor: &mut self
                            .settings_jstack_thread_name_filter_input
                            .selection_anchor,
                        marked_range: &mut self
                            .settings_jstack_thread_name_filter_input
                            .marked_range,
                        selection_drag: &mut self
                            .settings_jstack_thread_name_filter_input
                            .selection_drag,
                    },
                    &edit,
                );
                if edit.marked_range.is_none() {
                    self.config.log_display.jstack_thread_name_filters = self
                        .settings_jstack_thread_name_filter_input
                        .value
                        .trim()
                        .to_string();
                    self.rebuild_all_jstack_visible_row_caches();
                    self.placeholder_notice = "Jstack 线程名过滤已保存".to_string();
                    self.persist_config_or_report();
                }
            }
            AppTextInputTarget::SettingsJstackStackSegmentFilter => {
                apply_native_edit_to_parts(
                    NativeInputParts {
                        value: &mut self.settings_jstack_stack_segment_filter_input.value,
                        cursor: &mut self.settings_jstack_stack_segment_filter_input.cursor,
                        selection_anchor: &mut self
                            .settings_jstack_stack_segment_filter_input
                            .selection_anchor,
                        marked_range: &mut self
                            .settings_jstack_stack_segment_filter_input
                            .marked_range,
                        selection_drag: &mut self
                            .settings_jstack_stack_segment_filter_input
                            .selection_drag,
                    },
                    &edit,
                );
                if edit.marked_range.is_none() {
                    self.config.log_display.jstack_stack_segment_filters =
                        normalized_native_textarea_value(
                            &self.settings_jstack_stack_segment_filter_input.value,
                        );
                    self.rebuild_all_jstack_visible_row_caches();
                    self.placeholder_notice = "Jstack 线程段过滤已保存".to_string();
                    self.persist_config_or_report();
                }
            }
            AppTextInputTarget::SettingsUpgradeServer => {
                apply_native_edit_to_parts(
                    NativeInputParts {
                        value: &mut self.settings_upgrade_server_input.value,
                        cursor: &mut self.settings_upgrade_server_input.cursor,
                        selection_anchor: &mut self.settings_upgrade_server_input.selection_anchor,
                        marked_range: &mut self.settings_upgrade_server_input.marked_range,
                        selection_drag: &mut self.settings_upgrade_server_input.selection_drag,
                    },
                    &edit,
                );
                if edit.marked_range.is_none() {
                    self.config.upgrade.server_url =
                        self.settings_upgrade_server_input.value.trim().to_string();
                    self.placeholder_notice = "升级服务器已保存".to_string();
                    self.persist_config_or_report();
                }
            }
            AppTextInputTarget::SettingsUpgradePublicKey => {
                apply_native_edit_to_parts(
                    NativeInputParts {
                        value: &mut self.settings_upgrade_public_key_input.value,
                        cursor: &mut self.settings_upgrade_public_key_input.cursor,
                        selection_anchor: &mut self
                            .settings_upgrade_public_key_input
                            .selection_anchor,
                        marked_range: &mut self.settings_upgrade_public_key_input.marked_range,
                        selection_drag: &mut self.settings_upgrade_public_key_input.selection_drag,
                    },
                    &edit,
                );
                if edit.marked_range.is_none() {
                    self.config.upgrade.public_key_base64 = self
                        .settings_upgrade_public_key_input
                        .value
                        .trim()
                        .to_string();
                    self.placeholder_notice = "升级验签公钥已保存".to_string();
                    self.persist_config_or_report();
                }
            }
        }
    }

    /// 在不重置光标的前提下同步业务焦点，避免输入法提交写入旧输入框。
    fn focus_native_text_input_target(&mut self, target: AppTextInputTarget) {
        match target {
            AppTextInputTarget::SourceTreeSearch => {
                self.is_source_tree_search_focused = true;
            }
            AppTextInputTarget::SourcePickerPath => {
                self.source_picker.is_path_input_focused = true;
            }
            AppTextInputTarget::LogSearch(input_kind) => {
                self.log_search.keyword_input.is_focused =
                    input_kind == LogSearchInputKind::Keyword;
                self.log_search.directory_input.is_focused =
                    input_kind == LogSearchInputKind::Directory;
                if input_kind != LogSearchInputKind::Keyword {
                    self.log_search.keyword_input.marked_range = None;
                }
                if input_kind != LogSearchInputKind::Directory {
                    self.log_search.directory_input.marked_range = None;
                }
            }
            AppTextInputTarget::RuntimeFilter {
                analysis_id,
                input_kind,
            } => {
                self.focus_runtime_filter_input(analysis_id, input_kind);
            }
            AppTextInputTarget::SettingsQuickKeywords => {
                self.is_theme_dropdown_open = false;
                self.settings_quick_keywords_input.is_focused = true;
                self.settings_jstack_thread_name_filter_input.is_focused = false;
                self.settings_jstack_thread_name_filter_input.marked_range = None;
                self.settings_jstack_stack_segment_filter_input.is_focused = false;
                self.settings_jstack_stack_segment_filter_input.marked_range = None;
                self.settings_upgrade_server_input.is_focused = false;
                self.settings_upgrade_server_input.marked_range = None;
                self.settings_upgrade_public_key_input.is_focused = false;
                self.settings_upgrade_public_key_input.marked_range = None;
            }
            AppTextInputTarget::SettingsJstackThreadNameFilter => {
                self.is_theme_dropdown_open = false;
                self.settings_quick_keywords_input.is_focused = false;
                self.settings_quick_keywords_input.marked_range = None;
                self.settings_jstack_thread_name_filter_input.is_focused = true;
                self.settings_jstack_stack_segment_filter_input.is_focused = false;
                self.settings_jstack_stack_segment_filter_input.marked_range = None;
                self.settings_upgrade_server_input.is_focused = false;
                self.settings_upgrade_server_input.marked_range = None;
                self.settings_upgrade_public_key_input.is_focused = false;
                self.settings_upgrade_public_key_input.marked_range = None;
            }
            AppTextInputTarget::SettingsJstackStackSegmentFilter => {
                self.is_theme_dropdown_open = false;
                self.settings_quick_keywords_input.is_focused = false;
                self.settings_quick_keywords_input.marked_range = None;
                self.settings_jstack_thread_name_filter_input.is_focused = false;
                self.settings_jstack_thread_name_filter_input.marked_range = None;
                self.settings_jstack_stack_segment_filter_input.is_focused = true;
                self.settings_upgrade_server_input.is_focused = false;
                self.settings_upgrade_server_input.marked_range = None;
                self.settings_upgrade_public_key_input.is_focused = false;
                self.settings_upgrade_public_key_input.marked_range = None;
            }
            AppTextInputTarget::SettingsUpgradeServer => {
                self.is_theme_dropdown_open = false;
                self.settings_quick_keywords_input.is_focused = false;
                self.settings_quick_keywords_input.marked_range = None;
                self.settings_jstack_thread_name_filter_input.is_focused = false;
                self.settings_jstack_thread_name_filter_input.marked_range = None;
                self.settings_jstack_stack_segment_filter_input.is_focused = false;
                self.settings_jstack_stack_segment_filter_input.marked_range = None;
                self.settings_upgrade_server_input.is_focused = true;
                self.settings_upgrade_public_key_input.is_focused = false;
                self.settings_upgrade_public_key_input.marked_range = None;
            }
            AppTextInputTarget::SettingsUpgradePublicKey => {
                self.is_theme_dropdown_open = false;
                self.settings_quick_keywords_input.is_focused = false;
                self.settings_quick_keywords_input.marked_range = None;
                self.settings_jstack_thread_name_filter_input.is_focused = false;
                self.settings_jstack_thread_name_filter_input.marked_range = None;
                self.settings_jstack_stack_segment_filter_input.is_focused = false;
                self.settings_jstack_stack_segment_filter_input.marked_range = None;
                self.settings_upgrade_server_input.is_focused = false;
                self.settings_upgrade_server_input.marked_range = None;
                self.settings_upgrade_public_key_input.is_focused = true;
            }
        }
    }
}

/// 应用日志搜索窗口输入框的原生编辑，并维护关键字搜索缓存。
fn apply_native_log_search_edit(
    app: &mut ArgusApp,
    input_kind: LogSearchInputKind,
    edit: &NativeTextEdit,
) {
    let input = match input_kind {
        LogSearchInputKind::Keyword => &mut app.log_search.keyword_input,
        LogSearchInputKind::Directory => &mut app.log_search.directory_input,
    };
    apply_native_edit_to_parts(
        NativeInputParts {
            value: &mut input.value,
            cursor: &mut input.cursor,
            selection_anchor: &mut input.selection_anchor,
            marked_range: &mut input.marked_range,
            selection_drag: &mut input.selection_drag,
        },
        edit,
    );
    if input_kind == LogSearchInputKind::Keyword {
        app.clear_quick_log_search_state();
    }
}

/// 应用 Runtime 过滤输入框的原生编辑，并刷新当前分析页过滤结果。
fn apply_native_runtime_filter_edit(
    app: &mut ArgusApp,
    analysis_id: usize,
    input_kind: RuntimeFilterInputKind,
    edit: &NativeTextEdit,
) {
    let Some(input) = app.runtime_filter_input_mut(analysis_id, input_kind) else {
        return;
    };
    apply_native_edit_to_parts(
        NativeInputParts {
            value: &mut input.value,
            cursor: &mut input.cursor,
            selection_anchor: &mut input.selection_anchor,
            marked_range: &mut input.marked_range,
            selection_drag: &mut input.selection_drag,
        },
        edit,
    );
    if edit.marked_range.is_none() {
        app.after_runtime_filter_changed(analysis_id);
    }
}

/// 将原生输入编辑应用到单个输入框字段组。
fn apply_native_edit_to_parts(parts: NativeInputParts<'_>, edit: &NativeTextEdit) {
    let text_length = character_count(parts.value);
    let replacement_range = clamp_range(edit.replacement_range.clone(), text_length);
    let next_value = replace_character_range(parts.value, replacement_range, &edit.text);
    let next_length = character_count(&next_value);
    let selected_range = clamp_range(edit.selected_range.clone(), next_length);
    let marked_range = edit
        .marked_range
        .clone()
        .map(|range| clamp_range(range, next_length))
        .filter(|range| range.start < range.end);

    *parts.value = next_value;
    *parts.cursor = selected_range.end;
    *parts.selection_anchor =
        (selected_range.start != selected_range.end).then_some(selected_range.start);
    *parts.marked_range = marked_range;
    *parts.selection_drag = None;
}

/// 将字符范围夹在文本长度内，并确保起止顺序稳定。
fn clamp_range(range: Range<usize>, text_length: usize) -> Range<usize> {
    let start = range.start.min(text_length);
    let end = range.end.min(text_length);
    start.min(end)..start.max(end)
}

/// 清理设置输入框焦点态，保留文本和光标，便于再次点击时继续编辑。
fn clear_settings_input_focus(input: &mut crate::app::SettingsTextInputState) {
    input.is_focused = false;
    input.selection_anchor = None;
    input.marked_range = None;
    input.selection_drag = None;
}

/// 归一化原生输入提交的 textarea 文本，统一换行符并保留多行内容。
fn normalized_native_textarea_value(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use crate::app::{AppTextInputTarget, ArgusApp};
    use crate::text_selection::NativeTextEdit;

    /// 验证原生文本提交能写入中文并更新来源树搜索光标。
    #[test]
    fn native_text_input_applies_chinese_to_source_tree_search() {
        let mut app = ArgusApp::new();

        app.apply_native_text_input_edit(
            AppTextInputTarget::SourceTreeSearch,
            NativeTextEdit {
                replacement_range: 0..0,
                text: "中文".to_string(),
                selected_range: 2..2,
                marked_range: None,
            },
        );

        assert_eq!(app.source_tree_search_query, "中文");
        assert_eq!(app.source_tree_search_cursor, 2);
        assert!(app.source_tree_search_selection_anchor.is_none());
    }

    /// 验证设置输入框切换目标后，原生输入不会继续写入旧输入框。
    #[test]
    fn native_text_input_focuses_selected_settings_target() {
        let mut app = ArgusApp::new();
        app.settings_upgrade_server_input.value = "old".to_string();
        app.settings_upgrade_server_input.cursor = 3;

        app.apply_native_text_input_edit(
            AppTextInputTarget::SettingsUpgradeServer,
            NativeTextEdit {
                replacement_range: 3..3,
                text: "A".to_string(),
                selected_range: 4..4,
                marked_range: None,
            },
        );
        app.apply_native_text_input_edit(
            AppTextInputTarget::SettingsUpgradePublicKey,
            NativeTextEdit {
                replacement_range: 0..0,
                text: "B".to_string(),
                selected_range: 1..1,
                marked_range: None,
            },
        );

        assert_eq!(app.settings_upgrade_server_input.value, "oldA");
        assert_eq!(app.settings_upgrade_public_key_input.value, "B");
        assert!(!app.settings_upgrade_server_input.is_focused);
        assert!(app.settings_upgrade_public_key_input.is_focused);
    }

    /// 验证点击外部触发的统一失焦不会清空用户输入内容。
    #[test]
    fn clear_all_text_input_focus_preserves_values() {
        let mut app = ArgusApp::new();
        app.source_tree_search_query = "错误".to_string();
        app.source_tree_search_cursor = 2;
        app.source_tree_search_selection_anchor = Some(0);
        app.source_tree_search_marked_range = Some(0..2);
        app.is_source_tree_search_focused = true;
        app.settings_upgrade_server_input.value = "https://updates.example.com".to_string();
        app.settings_upgrade_server_input.cursor = 27;
        app.settings_upgrade_server_input.selection_anchor = Some(0);
        app.settings_upgrade_server_input.marked_range = Some(0..5);
        app.settings_upgrade_server_input.is_focused = true;
        app.log_search.keyword_input.value = "中文".to_string();
        app.log_search.keyword_input.cursor = 2;
        app.log_search.keyword_input.is_focused = true;

        app.clear_all_text_input_focus();

        assert_eq!(app.source_tree_search_query, "错误");
        assert_eq!(
            app.settings_upgrade_server_input.value,
            "https://updates.example.com"
        );
        assert_eq!(app.log_search.keyword_input.value, "中文");
        assert!(!app.is_source_tree_search_focused);
        assert!(!app.settings_upgrade_server_input.is_focused);
        assert!(!app.log_search.keyword_input.is_focused);
        assert!(app.source_tree_search_selection_anchor.is_none());
        assert!(app.settings_upgrade_server_input.selection_anchor.is_none());
        assert!(app.source_tree_search_marked_range.is_none());
        assert!(app.settings_upgrade_server_input.marked_range.is_none());
    }
}
