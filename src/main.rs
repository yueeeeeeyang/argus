//! 文件职责：Argus 桌面客户端启动入口。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：初始化 GPUI 应用、打开主窗口并处理应用重新打开事件。

use argus::app::ArgusApp;
use argus::assets::ArgusAssetSource;
use argus::fonts::register_argus_fonts;
use gpui::{
    App, AppContext, Application, Bounds, Keystroke, TitlebarOptions, WindowBounds, WindowOptions,
    point, px, size,
};

/// 启动 Argus GPUI 应用并创建透明原生标题栏的主窗口。
fn main() {
    let application = Application::new().with_assets(ArgusAssetSource::new());

    application.on_reopen(|cx| {
        if activate_last_available_window(cx) {
            return;
        }

        if let Err(error) = open_argus_main_window(cx) {
            eprintln!("重新打开 Argus 主窗口失败：{error}");
            return;
        }

        cx.activate(true);
    });

    application.run(|cx: &mut App| {
        if let Err(error) = register_argus_fonts(cx) {
            eprintln!("Argus 内置字体注册失败：{error}");
        }

        open_argus_main_window(cx).expect("打开 Argus 主窗口失败");

        cx.activate(true);
    });
}

/// 创建 Argus 主窗口；启动和 reopen 都复用同一套窗口选项，避免配置分叉。
fn open_argus_main_window(cx: &mut App) -> anyhow::Result<()> {
    let bounds = Bounds::centered(None, size(px(1330.0), px(820.0)), cx);

    let window_handle = cx.open_window(
        WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: None,
                appears_transparent: true,
                traffic_light_position: Some(point(px(20.0), px(14.0))),
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        },
        |_, cx| cx.new(|_| ArgusApp::new()),
    )?;
    observe_log_view_shortcuts(cx, window_handle);

    Ok(())
}

/// 前置观察主窗口日志阅读快捷键。
///
/// 说明：搜索窗口属于全局日志操作，只要已有日志标签即可触发；复制仍限制在日志正文焦点内，
/// 避免抢走输入框、下拉框等普通控件的复制快捷键。
fn observe_log_view_shortcuts(cx: &mut App, window_handle: gpui::WindowHandle<ArgusApp>) {
    let main_window_id = window_handle.window_id();
    cx.intercept_keystrokes(move |event, window, cx| {
        if window.window_handle().window_id() != main_window_id {
            return;
        }
        let is_search_shortcut = is_log_search_shortcut(&event.keystroke);
        let is_copy_shortcut = is_log_copy_shortcut(&event.keystroke);
        if !is_search_shortcut && !is_copy_shortcut {
            return;
        }

        let Some(Some(app_entity)) = window.root::<ArgusApp>() else {
            return;
        };
        let handled = app_entity.update(cx, |app, cx| {
            if is_search_shortcut {
                if !app.has_open_log_tab() {
                    return false;
                }
                app.open_log_search_window(cx);
                cx.notify();
                return true;
            }

            if is_copy_shortcut {
                if !app.is_active_log_view_focused() {
                    return false;
                }
                app.copy_active_log_text_selection(cx);
                cx.notify();
                return true;
            }

            false
        });

        if handled {
            cx.stop_propagation();
        }
    })
    .detach();
}

/// 判断是否为日志搜索快捷键；`secondary` 在 macOS 对应 Cmd，在 Windows/Linux 对应 Ctrl。
fn is_log_search_shortcut(keystroke: &Keystroke) -> bool {
    keystroke.modifiers.secondary() && keystroke.key.eq_ignore_ascii_case("f")
}

/// 判断是否为日志复制快捷键；这里补足日志阅读区子元素未获得键盘焦点时的复制路径。
fn is_log_copy_shortcut(keystroke: &Keystroke) -> bool {
    keystroke.modifiers.secondary() && keystroke.key.eq_ignore_ascii_case("c")
}

/// 激活平台记录的最后可用窗口；全部失效时返回 false，让调用方重新创建主窗口。
fn activate_last_available_window(cx: &mut App) -> bool {
    let Some(window_stack) = cx.window_stack() else {
        return false;
    };

    for window_handle in window_stack.into_iter().rev() {
        if window_handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            cx.activate(true);
            return true;
        }
    }

    false
}
