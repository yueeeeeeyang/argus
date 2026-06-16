//! 文件职责：Argus 桌面客户端启动入口。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-16
//! 作者：Argus 开发团队
//! 主要功能：初始化 GPUI 应用、打开主窗口、启动升级检查并处理应用重新打开和系统外部打开事件。

use argus::app::ArgusApp;
use argus::app::ExternalSourceTrigger;
use argus::assets::ArgusAssetSource;
use argus::fonts::register_argus_fonts;
use argus::platform::external_sources::{paths_from_open_urls, paths_from_startup_args};
use gpui::{
    App, AppContext, Application, Bounds, Keystroke, TitlebarOptions, WindowBounds, WindowHandle,
    WindowOptions, point, px, size,
};
use std::{cell::RefCell, path::PathBuf, rc::Rc};

/// 主窗口句柄共享引用；系统 open-url 监听和 reopen 逻辑共同维护最新可用窗口。
type MainWindowSlot = Rc<RefCell<Option<WindowHandle<ArgusApp>>>>;

/// 启动 Argus GPUI 应用并创建透明原生标题栏的主窗口。
fn main() {
    let application = Application::new().with_assets(ArgusAssetSource::new());
    let startup_paths = paths_from_startup_args(std::env::args_os().skip(1));
    let (external_open_sender, external_open_receiver) = async_channel::unbounded::<Vec<PathBuf>>();
    let main_window_slot: MainWindowSlot = Rc::new(RefCell::new(None));

    application.on_open_urls(move |urls| {
        let paths = paths_from_open_urls(urls);
        if !paths.is_empty() {
            let _ = external_open_sender.try_send(paths);
        }
    });

    application.on_reopen({
        let main_window_slot = main_window_slot.clone();
        move |cx| {
            if let Some(window_handle) = activate_last_argus_main_window(cx) {
                *main_window_slot.borrow_mut() = Some(window_handle);
                return;
            }

            match open_argus_main_window(cx) {
                Ok(window_handle) => {
                    *main_window_slot.borrow_mut() = Some(window_handle);
                }
                Err(error) => {
                    eprintln!("重新打开 Argus 主窗口失败：{error}");
                    return;
                }
            }

            cx.activate(true);
        }
    });

    application.run(move |cx: &mut App| {
        if let Err(error) = register_argus_fonts(cx) {
            eprintln!("Argus 内置字体注册失败：{error}");
        }

        let window_handle = open_argus_main_window(cx).expect("打开 Argus 主窗口失败");
        *main_window_slot.borrow_mut() = Some(window_handle);
        observe_external_open_requests(
            main_window_slot.clone(),
            external_open_receiver.clone(),
            cx,
        );
        if !startup_paths.is_empty() {
            load_external_paths_in_main_window(
                window_handle,
                startup_paths.clone(),
                ExternalSourceTrigger::StartupArgs,
                cx,
            );
        }
        start_upgrade_check_in_main_window(window_handle, cx);

        cx.activate(true);
    });
}

/// 创建 Argus 主窗口；启动和 reopen 都复用同一套窗口选项，避免配置分叉。
fn open_argus_main_window(cx: &mut App) -> anyhow::Result<WindowHandle<ArgusApp>> {
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

    Ok(window_handle)
}

/// 监听系统 Open With / open-url 事件，并转入主应用统一来源加载流程。
fn observe_external_open_requests(
    main_window_slot: MainWindowSlot,
    receiver: async_channel::Receiver<Vec<PathBuf>>,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        while let Ok(paths) = receiver.recv().await {
            let mut pending_paths = Some(paths);

            if let Some(window_handle) = *main_window_slot.borrow() {
                let update_result = window_handle.update(cx, |app, window, cx| {
                    window.activate_window();
                    if let Some(paths) = pending_paths.take() {
                        app.load_sources_from_paths(paths, ExternalSourceTrigger::OpenWith, cx);
                    }
                    cx.notify();
                });
                if update_result.is_ok() {
                    continue;
                }

                // 已缓存的主窗口句柄可能来自已经关闭的窗口；清空后用同一批路径重建主窗口加载。
                *main_window_slot.borrow_mut() = None;
            }

            let Some(paths) = pending_paths.take() else {
                continue;
            };
            let recreated_window = cx.update(open_argus_main_window);
            match recreated_window {
                Ok(Ok(window_handle)) => {
                    *main_window_slot.borrow_mut() = Some(window_handle);
                    let load_result = window_handle.update(cx, |app, window, cx| {
                        window.activate_window();
                        app.load_sources_from_paths(paths, ExternalSourceTrigger::OpenWith, cx);
                        cx.notify();
                    });
                    if let Err(error) = load_result {
                        *main_window_slot.borrow_mut() = None;
                        eprintln!("外部打开路径加载失败：新建 Argus 主窗口不可用：{error}");
                    }
                }
                Ok(Err(error)) => {
                    eprintln!("外部打开路径加载失败：重新创建 Argus 主窗口失败：{error}");
                }
                Err(error) => {
                    eprintln!("外部打开路径加载失败：无法回到 UI 线程创建主窗口：{error}");
                }
            }
        }
    })
    .detach();
}

/// 把启动参数或系统右键传入的路径加载到主窗口。
fn load_external_paths_in_main_window(
    window_handle: WindowHandle<ArgusApp>,
    paths: Vec<PathBuf>,
    trigger: ExternalSourceTrigger,
    cx: &mut App,
) {
    let _ = window_handle.update(cx, |app, window, cx| {
        window.activate_window();
        app.load_sources_from_paths(paths, trigger, cx);
        cx.notify();
    });
}

/// 在主窗口应用状态可用后启动自动升级检查；未启用或未配置服务器时应用层会直接跳过。
fn start_upgrade_check_in_main_window(window_handle: WindowHandle<ArgusApp>, cx: &mut App) {
    let _ = window_handle.update(cx, |app, _, cx| {
        app.start_upgrade_check(false, cx);
        cx.notify();
    });
}

/// 前置观察主窗口日志阅读快捷键。
///
/// 说明：搜索窗口属于全局日志操作，只要已有日志标签即可触发；F2 打点跳转只要求当前为已读取
/// 日志标签，避免日志正文子元素焦点丢失时快捷键失效；复制仍限制在日志正文焦点内，避免抢走输入框、
/// 下拉框等普通控件的复制快捷键。
fn observe_log_view_shortcuts(cx: &mut App, window_handle: gpui::WindowHandle<ArgusApp>) {
    let main_window_id = window_handle.window_id();
    cx.intercept_keystrokes(move |event, window, cx| {
        if window.window_handle().window_id() != main_window_id {
            return;
        }
        let is_search_shortcut = is_log_search_shortcut(&event.keystroke);
        let is_copy_shortcut = is_log_copy_shortcut(&event.keystroke);
        let is_marker_jump_shortcut = is_log_line_marker_jump_shortcut(&event.keystroke);
        log_key_probe_if_enabled(
            &event.keystroke,
            is_search_shortcut,
            is_copy_shortcut,
            is_marker_jump_shortcut,
        );
        if !is_search_shortcut && !is_copy_shortcut && !is_marker_jump_shortcut {
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

            if is_marker_jump_shortcut {
                app.jump_to_next_line_marker_from_viewport();
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

/// 判断是否为日志行号打点跳转快捷键；F2 不要求日志正文元素拥有 GPUI 焦点。
fn is_log_line_marker_jump_shortcut(keystroke: &Keystroke) -> bool {
    let key = keystroke.key.to_lowercase();
    key == "f2" && !keystroke.modifiers.modified()
}

/// 输出主窗口按键探针日志，帮助确认 macOS 是否真的把 F2 传给 GPUI。
///
/// 说明：该探针默认关闭，仅在设置 `ARGUS_KEY_DEBUG=1` 时输出，避免影响正常运行和终端日志。
fn log_key_probe_if_enabled(
    keystroke: &Keystroke,
    is_search_shortcut: bool,
    is_copy_shortcut: bool,
    is_marker_jump_shortcut: bool,
) {
    if std::env::var_os("ARGUS_KEY_DEBUG").is_none() {
        return;
    }

    eprintln!(
        "[argus-key] key={:?} modifiers={{cmd:{}, ctrl:{}, alt:{}, shift:{}, function:{}}} search={} copy={} marker_jump={}",
        keystroke.key,
        keystroke.modifiers.platform,
        keystroke.modifiers.control,
        keystroke.modifiers.alt,
        keystroke.modifiers.shift,
        keystroke.modifiers.function,
        is_search_shortcut,
        is_copy_shortcut,
        is_marker_jump_shortcut
    );
}

/// 激活最后一个 Argus 主窗口；全部失效时返回 `None`，让调用方重新创建主窗口。
fn activate_last_argus_main_window(cx: &mut App) -> Option<WindowHandle<ArgusApp>> {
    let Some(window_stack) = cx.window_stack() else {
        return None;
    };

    for window_handle in window_stack.into_iter().rev() {
        let Some(window_handle) = window_handle.downcast::<ArgusApp>() else {
            continue;
        };
        if window_handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            cx.activate(true);
            return Some(window_handle);
        }
    }

    None
}
