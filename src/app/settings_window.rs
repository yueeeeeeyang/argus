//! 文件职责：维护独立设置窗口的打开、置前和关闭状态。
//! 创建日期：2026-06-12
//! 修改日期：2026-06-12
//! 作者：Argus 开发团队
//! 主要功能：将设置页从标签页迁移到无标题栏独立窗口，同时复用主应用配置持久化逻辑。

use gpui::{AppContext, Bounds, Context, WindowBounds, WindowOptions, px, size};

use crate::app::ArgusApp;
use crate::ui::settings_window::SettingsWindow;

/// 设置窗口默认宽度，保证单页设置内容不过度拉伸。
const SETTINGS_WINDOW_WIDTH: f32 = 760.0;
/// 设置窗口默认高度。
const SETTINGS_WINDOW_HEIGHT: f32 = 560.0;
/// 设置窗口最小宽度。
const SETTINGS_WINDOW_MIN_WIDTH: f32 = 560.0;
/// 设置窗口最小高度。
const SETTINGS_WINDOW_MIN_HEIGHT: f32 = 420.0;

impl ArgusApp {
    /// 打开设置独立窗口；若窗口已存在，则直接激活并显示到最前。
    ///
    /// 参数说明：
    /// - `cx`：主应用上下文，用于创建或激活独立窗口。
    pub fn open_settings_window(&mut self, cx: &mut Context<Self>) {
        if self.is_settings_window_open {
            if let Some(window_handle) = self.settings_window_handle.clone()
                && window_handle
                    .update(cx, |_, window, _| window.activate_window())
                    .is_ok()
            {
                self.placeholder_notice = "设置窗口已显示到最前".to_string();
                return;
            }

            self.is_settings_window_open = false;
            self.settings_window_handle = None;
        }

        let app_entity = cx.entity();
        let initial_theme = self.theme.clone();
        let initial_snapshot = SettingsWindow::snapshot_from_app(self);
        let bounds = Bounds::centered(
            None,
            size(px(SETTINGS_WINDOW_WIDTH), px(SETTINGS_WINDOW_HEIGHT)),
            cx,
        );
        let window_options = WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(
                px(SETTINGS_WINDOW_MIN_WIDTH),
                px(SETTINGS_WINDOW_MIN_HEIGHT),
            )),
            ..Default::default()
        };

        self.is_settings_window_open = true;
        self.is_theme_dropdown_open = false;
        self.placeholder_notice = "已打开设置窗口".to_string();

        match cx.open_window(window_options, move |_, cx| {
            cx.new(|cx| SettingsWindow::new(app_entity, initial_theme, initial_snapshot, cx))
        }) {
            Ok(window_handle) => {
                self.settings_window_handle = Some(window_handle);
            }
            Err(error) => {
                self.is_settings_window_open = false;
                self.settings_window_handle = None;
                self.placeholder_notice = format!("打开设置窗口失败：{error}");
            }
        }
    }

    /// 清理设置窗口打开状态；窗口关闭按钮和系统关闭事件都走该入口。
    pub fn close_settings_window(&mut self) {
        self.is_settings_window_open = false;
        self.settings_window_handle = None;
        self.is_theme_dropdown_open = false;
        self.placeholder_notice = "已关闭设置窗口".to_string();
    }
}
