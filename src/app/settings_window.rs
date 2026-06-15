//! 文件职责：维护独立设置窗口的打开、置前和关闭状态。
//! 创建日期：2026-06-12
//! 修改日期：2026-06-12
//! 作者：Argus 开发团队
//! 主要功能：将设置页从标签页迁移到无标题栏独立窗口，同时复用主应用配置持久化逻辑。

use gpui::{AppContext, Bounds, Context, WindowBounds, WindowOptions, px, size};

use crate::app::ArgusApp;
use crate::platform::open_with_registration::{
    register_open_with, registration_status, unregister_open_with,
};
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
        self.refresh_open_with_registration_status(cx);

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

    /// 刷新系统“用 Argus 打开”右键菜单注册状态。
    ///
    /// 说明：状态查询应保持轻量，打开设置窗口和注册/卸载完成后都会调用；忙碌时跳过，
    /// 避免执行中状态被同步查询覆盖。
    pub fn refresh_open_with_registration_status(&mut self, _cx: &mut Context<Self>) {
        if self.is_open_with_registration_busy {
            return;
        }

        self.open_with_registration_status = registration_status();
    }

    /// 注册系统右键菜单；执行期间禁用注册/卸载按钮，避免重复写入系统状态。
    pub fn register_open_with_menu(&mut self, cx: &mut Context<Self>) {
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

    /// 卸载系统右键菜单；完成后重新查询系统状态并更新设置窗口提示。
    pub fn unregister_open_with_menu(&mut self, cx: &mut Context<Self>) {
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
}
