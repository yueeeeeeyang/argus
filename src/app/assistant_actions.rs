//! 文件职责：维护主窗口右侧 Agent 助手面板、尺寸和来源内容版本联动。
//! 创建日期：2026-07-16
//! 修改日期：2026-07-16
//! 作者：Argus 开发团队
//! 主要功能：延迟创建助手实体、控制展开收起与拖动、原子回填扫描结果，并在根日志重新加载时重建助手会话。

use gpui::{AppContext, Context, Window};

use crate::app::{
    ASSISTANT_PANEL_MAIN_CONTENT_MIN_WIDTH, ASSISTANT_PANEL_MAX_WIDTH, ASSISTANT_PANEL_MIN_WIDTH,
    ArgusApp, SettingsSection,
};
use crate::loader::SourceRegistry;
use crate::ui::assistant_panel::AssistantPanel;

impl ArgusApp {
    /// 切换右侧 Agent 助手面板；首次展开时才创建会话实体。
    pub(crate) fn toggle_assistant_panel(&mut self, cx: &mut Context<Self>) {
        let was_collapsed = self.is_assistant_panel_collapsed;
        if was_collapsed && self.assistant_panel.is_none() {
            let app = cx.entity();
            let theme = self.theme.clone();
            let ai_config = self.config.ai.clone();
            let has_loaded_sources = !self.source_registry.root_ids().is_empty();
            // 面板创建发生在 ArgusApp 更新事务内，所需状态必须按值注入，禁止构造器回读父实体。
            self.assistant_panel =
                Some(cx.new(move |cx| {
                    AssistantPanel::new(app, theme, ai_config, has_loaded_sources, cx)
                }));
        }
        self.is_assistant_panel_collapsed = !self.is_assistant_panel_collapsed;
        self.is_assistant_panel_resizing = false;
        self.is_assistant_resizer_hovered = false;
        self.assistant_panel_animation_generation =
            self.assistant_panel_animation_generation.wrapping_add(1);
        self.assistant_panel_animation_from_width = if was_collapsed {
            0.0
        } else {
            self.assistant_panel_width
        };
        self.assistant_panel_animation_to_width = if self.is_assistant_panel_collapsed {
            0.0
        } else {
            self.assistant_panel_width
        };
        self.placeholder_notice = if self.is_assistant_panel_collapsed {
            "已收起 Agent 助手".to_string()
        } else {
            "已展开 Agent 助手".to_string()
        };
    }

    /// 开始拖动助手面板左侧分割线。
    pub(crate) fn begin_assistant_panel_resize(&mut self, pointer_x: f32) {
        if self.is_assistant_panel_collapsed {
            return;
        }
        self.is_assistant_panel_resizing = true;
        self.assistant_resize_start_x = pointer_x;
        self.assistant_resize_start_width = self.assistant_panel_width;
        self.assistant_panel_animation_from_width = self.assistant_panel_width;
        self.assistant_panel_animation_to_width = self.assistant_panel_width;
    }

    /// 按窗口宽度调整助手面板，并始终为主内容保留最小可用宽度。
    pub(crate) fn resize_assistant_panel(&mut self, pointer_x: f32, window_width: f32) -> bool {
        if !self.is_assistant_panel_resizing {
            return false;
        }
        let dynamic_max = self.assistant_panel_max_width_for_window(window_width);
        let dynamic_min = ASSISTANT_PANEL_MIN_WIDTH.min(dynamic_max);
        let next_width = (self.assistant_resize_start_width + self.assistant_resize_start_x
            - pointer_x)
            .clamp(dynamic_min, dynamic_max);
        if (next_width - self.assistant_panel_width).abs() < 0.5 {
            return false;
        }
        self.assistant_panel_width = next_width;
        self.assistant_panel_animation_from_width = next_width;
        self.assistant_panel_animation_to_width = next_width;
        true
    }

    /// 结束助手面板宽度拖动。
    pub(crate) fn finish_assistant_panel_resize(&mut self) -> bool {
        let was_resizing = self.is_assistant_panel_resizing;
        self.is_assistant_panel_resizing = false;
        self.is_assistant_resizer_hovered = false;
        was_resizing
    }

    /// 按当前窗口宽度返回实际可渲染宽度，窗口缩小时不得挤占主内容的最低空间。
    pub(crate) fn current_assistant_panel_width_for_window(&self, window_width: f32) -> f32 {
        if self.is_assistant_panel_collapsed {
            return 0.0;
        }
        let dynamic_max = self.assistant_panel_max_width_for_window(window_width);
        let dynamic_min = ASSISTANT_PANEL_MIN_WIDTH.min(dynamic_max);
        self.assistant_panel_width.clamp(dynamic_min, dynamic_max)
    }

    /// 返回当前窗口和左侧来源栏共同约束下的助手最大宽度。
    ///
    /// 当窗口不足以同时容纳两个侧栏时，助手可以临时小于拖动下限甚至收缩为零；这样不会因
    /// 固定最小宽度进一步挤压日志正文。窗口恢复后仍使用保存的 `assistant_panel_width`。
    pub(crate) fn assistant_panel_max_width_for_window(&self, window_width: f32) -> f32 {
        let source_panel_width = if self.is_source_panel_collapsed {
            0.0
        } else {
            self.current_source_panel_width()
        };
        (window_width - source_panel_width - ASSISTANT_PANEL_MAIN_CONTENT_MIN_WIDTH)
            .clamp(0.0, ASSISTANT_PANEL_MAX_WIDTH)
    }

    /// 原子应用助手自己完成的全来源扫描，不向该助手发送外部来源变化重置事件。
    pub(crate) fn apply_assistant_scanned_registry(
        &mut self,
        expected_revision: u64,
        registry: SourceRegistry,
    ) -> Result<u64, String> {
        if self.source_content_revision != expected_revision {
            return Err("日志来源在扫描期间发生变化，请重新发送问题".to_string());
        }
        self.source_child_load_generations.clear();
        self.clear_source_archive_probe_state();
        self.source_registry = registry;
        self.rebuild_filtered_source_ids();
        self.source_content_revision = self.source_content_revision.wrapping_add(1);
        Ok(self.source_content_revision)
    }

    /// 标记来源注册表的非替换式补齐；推进并发版本，但保留助手现有上下文和可信快照。
    pub(crate) fn mark_source_content_changed(&mut self, cx: &mut Context<Self>) {
        self.source_content_revision = self.source_content_revision.wrapping_add(1);
        if let Some(panel) = self.assistant_panel.clone() {
            let revision = self.source_content_revision;
            let has_loaded_sources = !self.source_registry.root_ids().is_empty();
            panel.update(cx, |panel, panel_cx| {
                panel.accept_source_registry_revision(revision, has_loaded_sources, panel_cx);
                panel_cx.notify();
            });
        }
    }

    /// 根日志来源被完整替换后清空助手上下文，并延迟自动扫描新的全部来源。
    pub(crate) fn reset_assistant_after_log_reload(&mut self, cx: &mut Context<Self>) {
        self.source_content_revision = self.source_content_revision.wrapping_add(1);
        if let Some(panel) = self.assistant_panel.clone() {
            let revision = self.source_content_revision;
            let has_loaded_sources = !self.source_registry.root_ids().is_empty();
            panel.update(cx, |panel, panel_cx| {
                panel.reset_for_log_reload(revision, has_loaded_sources, panel_cx);
                panel_cx.notify();
            });
        }
    }

    /// 从助手不可用提示直接打开模型配置分类。
    pub(crate) fn open_assistant_model_settings(&mut self, cx: &mut Context<Self>) {
        self.selected_settings_section = SettingsSection::AiModel;
        self.open_settings_modal(cx);
    }

    /// 主窗口鼠标移动时同时处理左右两个可拖动侧栏。
    pub(crate) fn resize_panels_from_pointer(&mut self, pointer_x: f32, window: &Window) -> bool {
        let window_width = window.viewport_size().width / gpui::px(1.0);
        self.resize_source_panel(pointer_x) | self.resize_assistant_panel(pointer_x, window_width)
    }

    /// 鼠标释放时统一结束左右面板拖动。
    pub(crate) fn finish_panel_resizes(&mut self) -> bool {
        self.finish_source_panel_resize() | self.finish_assistant_panel_resize()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigManager;
    use crate::config::paths::isolated_test_dir;
    use gpui::TestAppContext;

    /// 验证首次展开时可在 ArgusApp 更新事务内安全创建面板，不会回读父实体触发 GPUI 重入 panic。
    #[gpui::test]
    fn opening_assistant_panel_does_not_reenter_parent_entity(cx: &mut TestAppContext) {
        let directory = isolated_test_dir("assistant-panel-open");
        let manager = ConfigManager::new(directory.join("settings.toml"));
        let app = cx.new(|_| ArgusApp::new_with_config_manager(manager));

        app.update(cx, |app, app_cx| {
            app.toggle_assistant_panel(app_cx);
        });

        let (is_collapsed, has_panel) = app.read_with(cx, |app, _| {
            (
                app.is_assistant_panel_collapsed,
                app.assistant_panel.is_some(),
            )
        });
        assert!(!is_collapsed);
        assert!(has_panel);
    }

    /// 验证助手默认收起且宽度调整始终为主内容保留空间。
    #[test]
    fn assistant_panel_defaults_collapsed_and_clamps_resize() {
        let directory = isolated_test_dir("assistant-panel-size");
        let manager = ConfigManager::new(directory.join("settings.toml"));
        let mut app = ArgusApp::new_with_config_manager(manager);
        assert!(app.is_assistant_panel_collapsed);
        assert_eq!(app.current_assistant_panel_width_for_window(1330.0), 0.0);

        app.is_assistant_panel_collapsed = false;
        app.assistant_panel_width = 350.0;
        app.begin_assistant_panel_resize(1000.0);
        assert!(app.resize_assistant_panel(900.0, 1330.0));
        assert_eq!(app.assistant_panel_width, 450.0);
        assert_eq!(app.current_assistant_panel_width_for_window(1330.0), 450.0);
        assert_eq!(app.current_assistant_panel_width_for_window(820.0), 0.0);

        app.is_source_panel_collapsed = true;
        assert_eq!(app.current_assistant_panel_width_for_window(820.0), 340.0);
    }

    /// 验证助手自己的来源回填原子推进基线，旧扫描结果无法覆盖更新后的来源树。
    #[test]
    fn assistant_source_scan_updates_revision_without_accepting_stale_result() {
        let directory = isolated_test_dir("assistant-source-revision");
        let manager = ConfigManager::new(directory.join("settings.toml"));
        let mut app = ArgusApp::new_with_config_manager(manager);

        let revision = app
            .apply_assistant_scanned_registry(0, SourceRegistry::new())
            .expect("当前版本的助手扫描结果应原子回填");
        assert_eq!(revision, 1);
        assert_eq!(app.source_content_revision, 1);
        assert!(
            app.apply_assistant_scanned_registry(0, SourceRegistry::new())
                .is_err(),
            "旧版本扫描结果不得覆盖更新后的来源树"
        );

        app.toggle_source_panel();
        assert_eq!(
            app.source_content_revision, 1,
            "纯界面折叠不应被误判为日志来源内容变化"
        );
    }
}
