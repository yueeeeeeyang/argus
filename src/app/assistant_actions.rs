//! 文件职责：维护主窗口右侧 Agent 助手面板、尺寸和来源内容版本联动。
//! 创建日期：2026-07-16
//! 修改日期：2026-09-24
//! 作者：Argus 开发团队
//! 主要功能：延迟创建助手实体、控制展开收起与拖动、在主窗口右侧空间充足时改用浮动窗口并跟随、原子回填扫描结果，并在根日志重新加载时重建助手会话。

use gpui::{
    AppContext, Bounds, Context, Pixels, TitlebarOptions, Window, WindowBounds, WindowOptions,
    point, px, size,
};

use crate::app::{
    ASSISTANT_PANEL_MAIN_CONTENT_MIN_WIDTH, ASSISTANT_PANEL_MAX_WIDTH, ASSISTANT_PANEL_MIN_WIDTH,
    ArgusApp, AssistantPanelMode, SettingsSection,
};
use crate::platform::custom_titlebar;
use crate::platform::window_follow::{
    attach_child_window_native, native_window_id, set_window_frame, window_frame_snapshot,
};
use crate::ui::assistant_float_window::AssistantFloatWindow;
use crate::ui::assistant_panel::AssistantPanel;

/// 浮动助手窗口与主窗口之间的缝隙宽度（像素）。
const ASSISTANT_FLOAT_WINDOW_GAP: f32 = 8.0;
/// 浮动助手窗口左侧原生红绿灯及其安全留白宽度，与标题栏占位一致。
const ASSISTANT_FLOAT_TRAFFIC_LIGHT_SAFE_WIDTH: f32 = 76.0;
/// 浮动助手窗口允许的最小高度，避免拖得过矮无法操作。
const ASSISTANT_FLOAT_WINDOW_MIN_HEIGHT: f32 = 400.0;

/// 计算浮动助手窗口的目标帧（纯几何，便于单测）。
///
/// 参数说明：
/// - `main`：主窗口帧 `(x, y, w, h)`（全局左上角坐标）。
/// - `screen`：主窗口所在屏幕帧 `(x, y, w, h)`（同一坐标系）。
/// - `panel_width`：助手面板期望宽度。
/// - `gap`：浮动窗口与主窗口之间的缝隙宽度。
///
/// 返回值：主窗口右侧剩余空间容纳 `panel_width + gap` 时，返回 `(x, y, w, h)` 目标帧
/// （与主窗口同顶同高、右缘留 `gap` 缝隙），否则返回 `None`。
fn assistant_float_frame(
    main: (f32, f32, f32, f32),
    screen: (f32, f32, f32, f32),
    panel_width: f32,
    gap: f32,
) -> Option<(f32, f32, f32, f32)> {
    let space_right = (screen.0 + screen.2) - (main.0 + main.2);
    (space_right >= panel_width + gap).then_some((
        main.0 + main.2 + gap,
        main.1,
        panel_width,
        main.3,
    ))
}

/// 把 GPUI 帧转换为便于纯函数计算和缓存比较的四元组。
fn frame_tuple(bounds: Bounds<Pixels>) -> (f32, f32, f32, f32) {
    (
        bounds.origin.x.into(),
        bounds.origin.y.into(),
        bounds.size.width.into(),
        bounds.size.height.into(),
    )
}

/// 判断两个帧的尺寸是否变化超过阈值；位置变化不计，
/// 供"父子窗口原生跟随时仅在尺寸变化时写原生帧"的判定复用。
fn assistant_float_frame_size_changed(
    previous: (f32, f32, f32, f32),
    next: (f32, f32, f32, f32),
) -> bool {
    (previous.2 - next.2).abs() > 0.5 || (previous.3 - next.3).abs() > 0.5
}

impl ArgusApp {
    /// 创建全新的助手会话实体。
    ///
    /// 面板创建发生在 ArgusApp 更新事务内，所需状态必须按值注入，禁止构造器回读父实体。
    fn create_assistant_panel(&mut self, cx: &mut Context<Self>) {
        let app = cx.entity();
        let theme = self.theme.clone();
        let ai_config = self.config.ai.clone();
        let has_loaded_sources = !self.source_registry.root_ids().is_empty();
        self.assistant_panel = Some(
            cx.new(move |cx| AssistantPanel::new(app, theme, ai_config, has_loaded_sources, cx)),
        );
    }

    /// 切换右侧 Agent 助手面板；首次展开时才创建会话实体。
    ///
    /// 展开时主窗口右侧空间充足则改为独立浮动窗口（与主窗口留 8px 缝隙并跟随主窗口），
    /// 空间不足、全屏、最大化或无窗口上下文时回退为现有内嵌展开动画；收起时先销毁
    /// 浮动窗口（如有），再走现有内嵌收起逻辑。
    pub(crate) fn toggle_assistant_panel(
        &mut self,
        window: Option<&Window>,
        cx: &mut Context<Self>,
    ) {
        let was_collapsed = self.is_assistant_panel_collapsed;
        if was_collapsed && self.assistant_panel.is_none() {
            self.create_assistant_panel(cx);
        }
        self.is_assistant_panel_collapsed = !self.is_assistant_panel_collapsed;
        self.is_assistant_panel_resizing = false;
        self.is_assistant_resizer_hovered = false;
        self.assistant_panel_animation_generation =
            self.assistant_panel_animation_generation.wrapping_add(1);
        if was_collapsed {
            let mut fallback_notice = None;
            if let Some(window) = window
                && let Some(frame) = self.assistant_float_frame_for(window)
            {
                match self.open_assistant_float_window(window, frame, cx) {
                    Ok(()) => {
                        // 浮动展开时主窗口不再为内嵌面板让位，动画宽度保持为 0。
                        self.assistant_panel_animation_from_width = 0.0;
                        self.assistant_panel_animation_to_width = 0.0;
                        self.placeholder_notice = "已展开浮动日志助手".to_string();
                        return;
                    }
                    Err(error) => {
                        fallback_notice = Some(format!("{error}，已回退为内嵌助手面板"));
                    }
                }
            }
            self.assistant_panel_animation_from_width = 0.0;
            self.assistant_panel_animation_to_width = self.assistant_panel_width;
            self.placeholder_notice =
                fallback_notice.unwrap_or_else(|| "已展开日志助手".to_string());
            return;
        }

        // 收起：先销毁浮动窗口（如有），再走内嵌收起逻辑。
        let was_floating = self.is_assistant_panel_floating();
        self.remove_assistant_float_window(None, true, cx);
        self.assistant_panel_animation_from_width = if was_floating {
            0.0
        } else {
            self.assistant_panel_width
        };
        self.assistant_panel_animation_to_width = 0.0;
        self.placeholder_notice = "已收起日志助手".to_string();
    }

    /// 主窗口空白区域获得点击时清除助手输入框焦点，让浮动窗口的高亮边框同步消失。
    pub(crate) fn clear_assistant_composer_focus(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.assistant_panel.clone() else {
            return;
        };
        panel.update(cx, |panel, panel_cx| {
            if panel.clear_composer_focus() {
                panel_cx.notify();
            }
        });
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
    ///
    /// 浮动展开时主窗口不再为内嵌面板让位，宽度恒为 0。
    pub(crate) fn current_assistant_panel_width_for_window(&self, window_width: f32) -> f32 {
        if self.is_assistant_panel_collapsed
            || self.assistant_panel_mode == AssistantPanelMode::Floating
        {
            return 0.0;
        }
        let dynamic_max = self.assistant_panel_max_width_for_window(window_width);
        let dynamic_min = ASSISTANT_PANEL_MIN_WIDTH.min(dynamic_max);
        self.assistant_panel_width.clamp(dynamic_min, dynamic_max)
    }

    /// 返回当前助手面板是否以浮动窗口展示。
    pub(crate) fn is_assistant_panel_floating(&self) -> bool {
        self.assistant_panel_mode == AssistantPanelMode::Floating
            && self.assistant_float_window.is_some()
    }

    /// 计算当前主窗口右侧能否容纳浮动助手窗口；不能或平台不支持时返回 `None`。
    ///
    /// 说明：主窗口全屏/最大化时没有可跟随的右侧空间；屏幕帧直接取自平台层快照，
    /// 因为 macOS 的 `cx.displays()` 原点恒为 (0,0)，无法据此定位窗口所在屏幕。
    pub(crate) fn assistant_float_frame_for(
        &self,
        window: &Window,
    ) -> Option<(f32, f32, f32, f32)> {
        if window.is_fullscreen() || window.is_maximized() {
            return None;
        }
        let snapshot = window_frame_snapshot(window)?;
        assistant_float_frame(
            frame_tuple(snapshot.window),
            frame_tuple(snapshot.screen),
            self.assistant_panel_width,
            ASSISTANT_FLOAT_WINDOW_GAP,
        )
    }

    /// 打开浮动助手窗口并记录句柄与同步帧缓存；失败时返回错误文本供调用方回退内嵌。
    fn open_assistant_float_window(
        &mut self,
        window: &Window,
        frame: (f32, f32, f32, f32),
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some(panel) = self.assistant_panel.clone() else {
            return Err("助手面板尚未创建".to_string());
        };
        let app_entity = cx.entity();
        let initial_theme = self.theme.clone();
        let frame_bounds = Bounds::new(
            point(px(frame.0), px(frame.1)),
            size(px(frame.2), px(frame.3)),
        );
        let frame_bounds_size = frame.2;
        // macOS 的 `WindowBounds::Windowed` 以目标屏幕为参照系；按主窗口所在屏幕换算并
        // 指定目标屏幕，打开后再用全局帧 setFrame 校正，双保险且同一更新事务内完成、不会闪跳。
        let snapshot = window_frame_snapshot(window);
        let display_id = window.display(cx).map(|display| display.id());
        let (open_bounds, open_display_id) = match (snapshot, display_id) {
            (Some(snapshot), Some(display_id)) => (
                Bounds::new(
                    point(
                        px(frame.0 - f32::from(snapshot.screen.origin.x)),
                        px(frame.1 - f32::from(snapshot.screen.origin.y)),
                    ),
                    frame_bounds.size,
                ),
                Some(display_id),
            ),
            _ => (frame_bounds, None),
        };
        let window_options = WindowOptions {
            // 与主窗口一致的原生红绿灯标题栏：关闭/最小化/最大化由系统按钮承担，
            // 浮动窗口内不再提供自定义关闭按钮。
            titlebar: Some(TitlebarOptions {
                title: None,
                appears_transparent: true,
                traffic_light_position: Some(point(px(20.0), px(14.0))),
            }),
            window_bounds: Some(WindowBounds::Windowed(open_bounds)),
            display_id: open_display_id,
            window_min_size: Some(size(
                px(ASSISTANT_PANEL_MIN_WIDTH),
                px(ASSISTANT_FLOAT_WINDOW_MIN_HEIGHT),
            )),
            ..Default::default()
        };

        match cx.open_window(window_options, move |_, cx| {
            cx.new(|cx| {
                AssistantFloatWindow::new(app_entity, panel, initial_theme, frame_bounds_size, cx)
            })
        }) {
            Ok(window_handle) => {
                // 打开后校正到全局帧：更新事务内持有 ArgusApp 借用，同步调用 AppKit
                // setFrame 可能经原生事件回调重入实体读取（曾导致崩溃），延迟到空闲时执行。
                // macOS 同时把浮动窗口挂为主窗口子窗口，后续拖动跟随由 AppKit 原生完成。
                let parent_native = native_window_id(window);
                self.assistant_float_native_follow = parent_native.is_some();
                let correction = frame_bounds;
                cx.defer(move |cx| {
                    let _ = window_handle.update(cx, |_, float_window, _| {
                        set_window_frame(float_window, correction);
                        if let Some(parent_native) = parent_native {
                            attach_child_window_native(parent_native, float_window);
                        }
                        // 打开即激活为 key 窗口：焦点和光标立即可用，
                        // 避免首次点击输入框迟迟不出现光标。
                        float_window.activate_window();
                        // 与主窗口同一连续点击监视：AppKit 不再对标题栏重复按下默认缩放，
                        // 仅拖拽空白处的双击触发应用内缩放。
                        custom_titlebar::register_repeated_click_guard(
                            float_window,
                            ASSISTANT_FLOAT_TRAFFIC_LIGHT_SAFE_WIDTH,
                        );
                    });
                });
                self.assistant_panel_mode = AssistantPanelMode::Floating;
                self.assistant_float_window = Some(window_handle);
                self.assistant_float_synced_frame = Some(frame);
                Ok(())
            }
            Err(error) => Err(format!("打开浮动日志助手失败：{error}")),
        }
    }

    /// 主窗口渲染期间同步浮动助手窗口位置：主窗口移动/缩放后跟随，空间不足时回退内嵌。
    pub(crate) fn sync_assistant_float_window(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.assistant_panel_mode != AssistantPanelMode::Floating {
            return;
        }
        let Some(window_handle) = self.assistant_float_window else {
            return;
        };
        let Some(frame) = self.assistant_float_frame_for(window) else {
            self.remove_assistant_float_window(
                Some("主窗口空间不足，已切换为内嵌助手面板"),
                false,
                cx,
            );
            return;
        };
        let previous = self.assistant_float_synced_frame;
        if previous == Some(frame) {
            return;
        }
        self.assistant_float_synced_frame = Some(frame);
        // macOS 父子窗口已由 AppKit 原生跟随位置；只有尺寸变化（主窗口缩放或面板宽度
        // 变化）才需要写原生帧，拖动主窗口时不再逐帧调用 setFrame，消除跟随卡顿。
        if self.assistant_float_native_follow
            && previous.is_some_and(|old| !assistant_float_frame_size_changed(old, frame))
        {
            return;
        }
        let app_entity = cx.entity();
        let frame_bounds = Bounds::new(
            point(px(frame.0), px(frame.1)),
            size(px(frame.2), px(frame.3)),
        );
        // 渲染/更新期间持有 ArgusApp 借用，同步调用 AppKit setFrame 可能经原生事件
        // 回调重入实体读取（曾导致崩溃），延迟到事件循环空闲时执行。
        cx.defer(move |cx| {
            let applied = window_handle
                .update(cx, |_, float_window, _| {
                    set_window_frame(float_window, frame_bounds)
                })
                .unwrap_or(false);
            if !applied {
                app_entity.update(cx, |app, cx| {
                    app.remove_assistant_float_window(
                        Some("主窗口空间不足，已切换为内嵌助手面板"),
                        false,
                        cx,
                    );
                });
            }
        });
    }

    /// 销毁浮动助手窗口并清理浮动状态；需要应用上下文的关闭路径统一走这里。
    fn remove_assistant_float_window(
        &mut self,
        notice: Option<&str>,
        keep_expanded: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(window_handle) = self.assistant_float_window.take() {
            // 窗口销毁延迟到事件循环空闲时执行，避免在实体借用期间触发原生窗口回调。
            cx.defer(move |cx| {
                let _ = window_handle.update(cx, |_, window, _| {
                    custom_titlebar::clear_repeated_click_guard(window);
                    window.remove_window();
                });
            });
        }
        self.close_assistant_float_window(notice, keep_expanded);
    }

    /// 清理浮动助手状态：模式回退内嵌、清同步帧缓存并按需收起面板。
    ///
    /// 只清理应用状态，不销毁原生窗口；窗口销毁由持有应用上下文的路径调用
    /// `remove_assistant_float_window`，或由系统关闭流程（红绿灯）自行完成。
    /// 无浮动窗口时调用幂等。
    pub(crate) fn close_assistant_float_window(
        &mut self,
        notice: Option<&str>,
        keep_expanded: bool,
    ) {
        self.assistant_float_window = None;
        self.assistant_panel_mode = AssistantPanelMode::Docked;
        self.assistant_float_synced_frame = None;
        self.assistant_float_native_follow = false;
        if !keep_expanded {
            self.is_assistant_panel_collapsed = true;
        }
        // 回到内嵌布局时动画宽度与目标状态对齐，避免面板宽度跳变。
        self.assistant_panel_animation_generation =
            self.assistant_panel_animation_generation.wrapping_add(1);
        self.assistant_panel_animation_from_width = if keep_expanded {
            self.assistant_panel_width
        } else {
            0.0
        };
        self.assistant_panel_animation_to_width = self.assistant_panel_animation_from_width;
        if let Some(notice) = notice {
            self.placeholder_notice = notice.to_string();
        }
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

    /// 根日志来源被完整替换后销毁旧助手会话；面板展开时立即按当前来源状态创建全新会话。
    ///
    /// 助手会话与加载的日志同生命周期：旧实体直接销毁，`Drop` 会取消全部后台任务和
    /// 流式请求；面板收起时不提前创建，等下次展开再按最新来源状态创建。
    /// 浮动展开时先关闭浮动窗口（其持有旧面板实体），再按内嵌方式重建，避免旧会话残留。
    pub(crate) fn reset_assistant_after_log_reload(&mut self, cx: &mut Context<Self>) {
        self.source_content_revision = self.source_content_revision.wrapping_add(1);
        if self.is_assistant_panel_floating() {
            self.remove_assistant_float_window(None, true, cx);
        }
        self.assistant_panel = None;
        if !self.is_assistant_panel_collapsed {
            self.create_assistant_panel(cx);
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
            app.toggle_assistant_panel(None, app_cx);
        });

        let (is_collapsed, has_panel, is_floating) = app.read_with(cx, |app, _| {
            (
                app.is_assistant_panel_collapsed,
                app.assistant_panel.is_some(),
                app.assistant_panel_mode,
            )
        });
        assert!(!is_collapsed);
        assert!(has_panel);
        // 无窗口上下文时一律回退为内嵌面板。
        assert_eq!(is_floating, AssistantPanelMode::Docked);
    }

    /// 验证日志重新加载会销毁旧助手会话实体：面板展开时立即创建全新会话，收起时延迟到下次展开。
    #[gpui::test]
    fn log_reload_recreates_assistant_session(cx: &mut TestAppContext) {
        let directory = isolated_test_dir("assistant-session-recreate");
        let manager = ConfigManager::new(directory.join("settings.toml"));
        let app = cx.new(|_| ArgusApp::new_with_config_manager(manager));

        let first_panel_id = app.update(cx, |app, app_cx| {
            app.toggle_assistant_panel(None, app_cx);
            let panel_id = app.assistant_panel.as_ref().map(|panel| panel.entity_id());
            app.reset_assistant_after_log_reload(app_cx);
            panel_id
        });
        let second_panel_id = app.read_with(cx, |app, _| {
            app.assistant_panel.as_ref().map(|panel| panel.entity_id())
        });
        assert!(first_panel_id.is_some());
        assert!(second_panel_id.is_some());
        assert_ne!(
            first_panel_id, second_panel_id,
            "重新加载日志后应销毁旧会话并创建全新的助手会话实体"
        );

        // 面板收起时只销毁不重建，下次展开再按最新来源状态创建。
        app.update(cx, |app, app_cx| {
            app.is_assistant_panel_collapsed = true;
            app.reset_assistant_after_log_reload(app_cx);
        });
        let has_panel = app.read_with(cx, |app, _| app.assistant_panel.is_some());
        assert!(
            !has_panel,
            "面板收起时重新加载只销毁旧会话，不提前创建新会话"
        );
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

    /// 验证纯界面折叠不会被误判为日志来源内容变化。
    #[test]
    fn assistant_panel_toggle_keeps_source_revision() {
        let directory = isolated_test_dir("assistant-source-revision");
        let manager = ConfigManager::new(directory.join("settings.toml"));
        let mut app = ArgusApp::new_with_config_manager(manager);
        assert_eq!(app.source_content_revision, 0);

        app.toggle_source_panel();
        assert_eq!(
            app.source_content_revision, 0,
            "纯界面折叠不应被误判为日志来源内容变化"
        );
    }

    /// 验证空间充足时浮动窗口帧贴在主窗口右侧 8px 缝隙处，且与主窗口同顶同高。
    #[test]
    fn assistant_float_frame_places_window_right_of_main() {
        let frame = assistant_float_frame(
            (100.0, 120.0, 800.0, 600.0),
            (0.0, 0.0, 2560.0, 1440.0),
            350.0,
            8.0,
        );
        assert_eq!(frame, Some((908.0, 120.0, 350.0, 600.0)));
    }

    /// 验证临界值：右侧空间恰好等于面板宽加缝隙时仍可浮动，差 1px 则回退内嵌。
    #[test]
    fn assistant_float_frame_checks_space_threshold() {
        // 主窗口右缘 2000，屏幕右缘 2358：空间 358 == 350 + 8。
        let exact_fit = assistant_float_frame(
            (1200.0, 0.0, 800.0, 900.0),
            (0.0, 0.0, 2358.0, 1440.0),
            350.0,
            8.0,
        );
        assert_eq!(exact_fit, Some((2008.0, 0.0, 350.0, 900.0)));

        // 屏幕右缘 2357：空间 357 < 350 + 8。
        let narrow = assistant_float_frame(
            (1200.0, 0.0, 800.0, 900.0),
            (0.0, 0.0, 2357.0, 1440.0),
            350.0,
            8.0,
        );
        assert_eq!(narrow, None);
    }

    /// 验证主窗口跨屏时按包含其中心点的屏幕计算右侧空间。
    #[test]
    fn assistant_float_frame_uses_screen_containing_main_window() {
        // 主窗口横跨两块屏幕边界，中心点 (2000, 500) 落在右侧屏幕 B 内。
        let frame = assistant_float_frame(
            (1800.0, 100.0, 400.0, 800.0),
            (1920.0, 0.0, 1920.0, 1080.0),
            350.0,
            8.0,
        );
        assert_eq!(frame, Some((2208.0, 100.0, 350.0, 800.0)));
    }

    /// 验证浮动展开时主窗口不再为内嵌面板让位，渲染宽度恒为 0。
    #[test]
    fn assistant_panel_width_is_zero_when_floating() {
        let directory = isolated_test_dir("assistant-panel-floating-width");
        let manager = ConfigManager::new(directory.join("settings.toml"));
        let mut app = ArgusApp::new_with_config_manager(manager);
        app.is_assistant_panel_collapsed = false;
        app.assistant_panel_width = 350.0;
        app.assistant_panel_mode = AssistantPanelMode::Floating;

        assert_eq!(app.current_assistant_panel_width_for_window(1330.0), 0.0);
    }

    /// 验证无浮动窗口时重复清理状态幂等，且 keep_expanded 语义正确。
    #[test]
    fn closing_float_window_without_handle_is_idempotent() {
        let directory = isolated_test_dir("assistant-float-close");
        let manager = ConfigManager::new(directory.join("settings.toml"));
        let mut app = ArgusApp::new_with_config_manager(manager);

        app.close_assistant_float_window(Some("第一次清理"), false);
        app.close_assistant_float_window(Some("第二次清理"), true);
        assert_eq!(app.assistant_panel_mode, AssistantPanelMode::Docked);
        assert!(app.assistant_float_synced_frame.is_none());
        // keep_expanded=false 收起面板，后续 keep_expanded=true 的幂等调用不改变收起状态。
        assert!(app.is_assistant_panel_collapsed);
        assert_eq!(app.placeholder_notice, "第二次清理");

        app.is_assistant_panel_collapsed = false;
        app.close_assistant_float_window(None, true);
        assert!(!app.is_assistant_panel_collapsed);
        assert!(!app.assistant_float_native_follow);
    }

    /// 验证帧尺寸变化判定只关心宽高：纯位置移动不触发原生帧写入。
    #[test]
    fn float_frame_size_change_ignores_origin_moves() {
        let base = (100.0, 200.0, 400.0, 800.0);
        assert!(!assistant_float_frame_size_changed(
            base,
            (500.0, 900.0, 400.0, 800.0)
        ));
        assert!(assistant_float_frame_size_changed(
            base,
            (100.0, 200.0, 420.0, 800.0)
        ));
        assert!(assistant_float_frame_size_changed(
            base,
            (100.0, 200.0, 400.0, 700.0)
        ));
        assert!(!assistant_float_frame_size_changed(
            base,
            (100.0, 200.0, 400.4, 800.4)
        ));
    }
}
