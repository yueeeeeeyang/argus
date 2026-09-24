//! 文件职责：封装主窗口自定义标题栏与操作系统原生窗口拖动行为。
//! 创建日期：2026-07-14
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：按平台协调透明标题栏命中、系统窗口拖动和重复点击行为，并显式驱动 Windows HWND 移动。

use gpui::Window;

/// 配置主窗口原生内容视图，使标签和按钮不再被 macOS 当作可拖拽标题栏。
///
/// 参数说明：
/// - `window`：已完成原生创建的 GPUI 主窗口。
/// - `titlebar_height`：自定义标题栏高度，用于限制原生连续点击拦截范围。
/// - `native_control_safe_width`：窗口左侧原生交通灯及其安全留白宽度。
///
/// 返回值：配置成功返回 `Ok(())`；无法取得或调整原生视图时返回可展示的错误文本。
#[cfg(target_os = "macos")]
pub(crate) fn configure_main_window(
    window: &Window,
    titlebar_height: f32,
    native_control_safe_width: f32,
) -> Result<(), String> {
    macos::configure_main_window(window, titlebar_height, native_control_safe_width)
}

/// 非 macOS 平台无需调整原生内容视图。
#[cfg(not(target_os = "macos"))]
pub(crate) fn configure_main_window(
    _window: &Window,
    _titlebar_height: f32,
    _native_control_safe_width: f32,
) -> Result<(), String> {
    Ok(())
}

/// 从已经确认为空白的标题栏区域发起系统级窗口拖动。
///
/// 参数说明：
/// - `window`：当前 GPUI 主窗口。
///
/// macOS 使用当前原生鼠标按下事件调用 Window Server；Windows 向当前 HWND 发送原生
/// 标题栏拖动消息；Linux/BSD 请求合成器开始移动。
#[cfg(target_os = "macos")]
pub(crate) fn start_window_drag(window: &Window) {
    macos::start_window_drag(window);
}

/// Windows 直接向当前 HWND 发送标题栏按下消息，绕过 GPUI 0.2.2 尚未实现的移动 API。
#[cfg(target_os = "windows")]
pub(crate) fn start_window_drag(window: &Window) {
    windows::start_window_drag(window);
}

/// Linux/BSD 的客户端标题栏需要显式请求窗口系统开始移动。
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn start_window_drag(window: &Window) {
    window.start_window_move();
}

/// 注册窗口到连续点击监视器：macOS 上取消 AppKit 对重复按下的默认缩放分发，
/// 仅保留应用内拖拽空白处的双击缩放；非 macOS 平台无需处理。
#[cfg(target_os = "macos")]
pub(crate) fn register_repeated_click_guard(window: &Window, native_control_safe_width: f32) {
    macos::register_repeated_click_guard(window, native_control_safe_width);
}

/// 非 macOS 平台没有标题栏默认缩放问题，注册为空操作。
#[cfg(not(target_os = "macos"))]
pub(crate) fn register_repeated_click_guard(_window: &Window, _native_control_safe_width: f32) {}

/// 窗口关闭前清除其连续点击监视槽位，避免悬空原生指针被复用后误拦事件。
#[cfg(target_os = "macos")]
pub(crate) fn clear_repeated_click_guard(window: &Window) {
    macos::clear_repeated_click_guard(window);
}

/// 非 macOS 平台为空操作。
#[cfg(not(target_os = "macos"))]
pub(crate) fn clear_repeated_click_guard(_window: &Window) {}

#[cfg(target_os = "windows")]
mod windows {
    use gpui::Window;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::{
        Foundation::HWND,
        UI::{
            Input::KeyboardAndMouse::ReleaseCapture,
            WindowsAndMessaging::{HTCAPTION, SendMessageW, WM_NCLBUTTONDOWN},
        },
    };

    /// 释放当前鼠标捕获并让 Windows 进入标准标题栏移动循环。
    ///
    /// 参数说明：
    /// - `window`：当前 GPUI 主窗口，用于取得精确的 Win32 HWND。
    ///
    /// 说明：GPUI 0.2.2 的 Windows `start_window_move` 是空实现，透明标题栏的
    /// `WindowControlArea::Drag` 在部分环境也不会进入系统移动循环，因此这里直接复用
    /// Win32 客户端自绘标题栏的标准做法。
    pub(super) fn start_window_drag(window: &Window) {
        let Ok(window_handle) = HasWindowHandle::window_handle(window) else {
            return;
        };
        let RawWindowHandle::Win32(handle) = window_handle.as_raw() else {
            return;
        };
        let hwnd = handle.hwnd.get() as HWND;

        unsafe {
            let _ = ReleaseCapture();
            let _ = SendMessageW(hwnd, WM_NCLBUTTONDOWN, HTCAPTION as usize, 0);
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use block::{Block, ConcreteBlock};
    use gpui::Window;
    use objc::runtime::{
        BOOL, Class, Imp, NO, Object, Sel, class_addMethod, objc_allocateClassPair, objc_getClass,
        objc_registerClassPair, object_getClass, sel_registerName,
    };
    use objc2::{msg_send, runtime::AnyObject};
    use objc2_foundation::{NSPoint, NSRect};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::{
        ffi::c_char,
        mem, ptr,
        sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering},
    };

    /// 仅供 Argus 主窗口原生内容视图使用的 Objective-C 子类名。
    const MAIN_WINDOW_VIEW_CLASS_NAME: &[u8] = b"ArgusMainWindowGPUIView\0";
    /// AppKit 左键按下事件掩码，枚举值为 `1 << NSEventTypeLeftMouseDown`。
    const LEFT_MOUSE_DOWN_EVENT_MASK: usize = 1 << 1;
    /// 保留给 macOS 原生窗口缩放命中的边缘宽度。
    const WINDOW_RESIZE_EDGE_INSET: f64 = 5.0;
    /// 当前 Argus 主窗口原生对象，由进程级事件监视器读取。
    static MAIN_NATIVE_WINDOW: AtomicPtr<Object> = AtomicPtr::new(ptr::null_mut());
    /// 当前 Argus 主窗口 GPUI 内容视图，由进程级事件监视器直接接收第二次按下。
    static MAIN_NATIVE_VIEW: AtomicPtr<Object> = AtomicPtr::new(ptr::null_mut());
    /// 当前浮动助手窗口原生对象；为空表示浮动窗口未注册。
    static FLOAT_NATIVE_WINDOW: AtomicPtr<Object> = AtomicPtr::new(ptr::null_mut());
    /// 当前浮动助手窗口 GPUI 内容视图。
    static FLOAT_NATIVE_VIEW: AtomicPtr<Object> = AtomicPtr::new(ptr::null_mut());
    /// 浮动窗口左侧原生按钮安全宽度的 IEEE 754 位表示。
    static FLOAT_SAFE_WIDTH_BITS: AtomicU32 = AtomicU32::new(0);
    /// 自定义标题栏高度的 IEEE 754 位表示，由事件监视器无锁读取。
    static TITLEBAR_HEIGHT_BITS: AtomicU32 = AtomicU32::new(0);
    /// 左侧原生窗口按钮安全宽度的 IEEE 754 位表示。
    static NATIVE_CONTROL_SAFE_WIDTH_BITS: AtomicU32 = AtomicU32::new(0);
    /// 本地事件监视器只安装一次；主窗口重建时仅更新上面两个目标指针。
    static IS_REPEATED_CLICK_MONITOR_INSTALLED: AtomicBool = AtomicBool::new(false);

    unsafe extern "C" {
        /// Objective-C 0.2 未公开 `object_setClass`，这里直接绑定运行时函数以仅调整当前实例。
        fn object_setClass(object: *mut Object, class: *const Class) -> *const Class;
        /// 直接绑定 Objective-C 消息派发入口，避免旧版宏向本项目注入历史 cfg 告警。
        fn objc_msgSend();
    }

    /// 覆盖 `NSView.mouseDownCanMoveWindow`，禁止非透明标题栏命中穿透到原生窗口。
    extern "C" fn mouse_down_can_move_window(_: &Object, _: Sel) -> BOOL {
        NO
    }

    /// 描述当前重复点击相对原生内容视图的位置和标题栏约束。
    #[derive(Clone, Copy, Debug)]
    struct RepeatedClickHitTest {
        location_x: f64,
        location_y: f64,
        view_width: f64,
        view_height: f64,
        titlebar_height: f64,
        native_control_safe_width: f64,
    }

    /// 配置主窗口视图的实例专用子类，避免修改 GPUI 全局视图类而影响其他独立窗口。
    pub(super) fn configure_main_window(
        window: &Window,
        titlebar_height: f32,
        native_control_safe_width: f32,
    ) -> Result<(), String> {
        if !titlebar_height.is_finite() || titlebar_height <= WINDOW_RESIZE_EDGE_INSET as f32 {
            return Err("自定义标题栏高度无效".to_string());
        }
        if !native_control_safe_width.is_finite() || native_control_safe_width < 0.0 {
            return Err("原生窗口按钮安全宽度无效".to_string());
        }
        TITLEBAR_HEIGHT_BITS.store(titlebar_height.to_bits(), Ordering::Release);
        NATIVE_CONTROL_SAFE_WIDTH_BITS
            .store(native_control_safe_width.to_bits(), Ordering::Release);
        let native_view = native_view(window)?;

        unsafe {
            let superclass = object_getClass(native_view);
            if superclass.is_null() {
                return Err("无法读取 GPUI 主窗口原生视图类".to_string());
            }

            let class_name = MAIN_WINDOW_VIEW_CLASS_NAME.as_ptr().cast::<c_char>();
            let mut subclass = objc_getClass(class_name);
            if subclass.is_null() {
                subclass = objc_allocateClassPair(superclass, class_name, 0);
                if subclass.is_null() {
                    return Err("无法创建 Argus 主窗口原生视图子类".to_string());
                }

                // Apple Silicon 的 Objective-C BOOL 编码为 `B`；Intel macOS 编码为 `c`。
                #[cfg(target_arch = "aarch64")]
                let method_types = b"B@:\0";
                #[cfg(not(target_arch = "aarch64"))]
                let method_types = b"c@:\0";
                let implementation: Imp = mem::transmute::<extern "C" fn(&Object, Sel) -> BOOL, Imp>(
                    mouse_down_can_move_window,
                );
                let method_added = class_addMethod(
                    subclass.cast_mut(),
                    sel_registerName(c"mouseDownCanMoveWindow".as_ptr()),
                    implementation,
                    method_types.as_ptr().cast::<c_char>(),
                );
                if method_added == NO {
                    return Err("无法覆盖主窗口原生拖动区域判定".to_string());
                }
                objc_registerClassPair(subclass.cast_mut());
            }

            object_setClass(native_view, subclass);
            let send_bool_message: unsafe extern "C" fn(*mut Object, Sel) -> BOOL =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let can_move_window = send_bool_message(
                native_view,
                sel_registerName(c"mouseDownCanMoveWindow".as_ptr()),
            );
            if can_move_window != NO {
                return Err("主窗口原生视图仍会把交互区当作可拖拽标题栏".to_string());
            }

            update_double_click_monitor_target(native_view)?;
        }

        Ok(())
    }

    /// 更新事件监视器目标；首次调用时安装进程级 AppKit 本地事件监视器。
    unsafe fn update_double_click_monitor_target(native_view: *mut Object) -> Result<(), String> {
        unsafe {
            let send_object_message: unsafe extern "C" fn(*mut Object, Sel) -> *mut Object =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let native_window =
                send_object_message(native_view, sel_registerName(c"window".as_ptr()));
            if native_window.is_null() {
                return Err("无法取得 Argus 主窗口原生 NSWindow".to_string());
            }

            MAIN_NATIVE_WINDOW.store(native_window, Ordering::Release);
            MAIN_NATIVE_VIEW.store(native_view, Ordering::Release);
            if IS_REPEATED_CLICK_MONITOR_INSTALLED
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
                && let Err(error) = install_repeated_click_monitor()
            {
                IS_REPEATED_CLICK_MONITOR_INSTALLED.store(false, Ordering::Release);
                return Err(error);
            }
        }

        Ok(())
    }

    /// 注册一个需要连续点击监视的浮动窗口；安全区取窗口左侧原生按钮占位宽度。
    pub(super) fn register_repeated_click_guard(window: &Window, native_control_safe_width: f32) {
        let Ok(native_view) = native_view(window) else {
            return;
        };
        unsafe {
            let send_object_message: unsafe extern "C" fn(*mut Object, Sel) -> *mut Object =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let native_window =
                send_object_message(native_view, sel_registerName(c"window".as_ptr()));
            if native_window.is_null() {
                return;
            }
            FLOAT_NATIVE_WINDOW.store(native_window, Ordering::Release);
            FLOAT_NATIVE_VIEW.store(native_view, Ordering::Release);
            FLOAT_SAFE_WIDTH_BITS.store(native_control_safe_width.to_bits(), Ordering::Release);
            // 监视器全进程只装一次；主窗口未配置时由浮动窗口触发安装。
            if IS_REPEATED_CLICK_MONITOR_INSTALLED
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
                && let Err(error) = install_repeated_click_monitor()
            {
                IS_REPEATED_CLICK_MONITOR_INSTALLED.store(false, Ordering::Release);
                eprintln!("安装连续点击监视器失败：{error}");
            }
        }
    }

    /// 浮动窗口关闭时清除其连续点击监视槽位，避免悬空的原生指针被复用后误拦事件。
    pub(super) fn clear_repeated_click_guard(window: &Window) {
        let Ok(native_view) = native_view(window) else {
            return;
        };
        unsafe {
            let send_object_message: unsafe extern "C" fn(*mut Object, Sel) -> *mut Object =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let native_window =
                send_object_message(native_view, sel_registerName(c"window".as_ptr()));
            if !native_window.is_null()
                && native_window == FLOAT_NATIVE_WINDOW.load(Ordering::Acquire)
            {
                FLOAT_NATIVE_WINDOW.store(ptr::null_mut(), Ordering::Release);
                FLOAT_NATIVE_VIEW.store(ptr::null_mut(), Ordering::Release);
                FLOAT_SAFE_WIDTH_BITS.store(0, Ordering::Release);
            }
        }
    }

    /// 安装仅监听左键按下的本地事件监视器，取消主窗口重复按下的 AppKit 默认分发。
    unsafe fn install_repeated_click_monitor() -> Result<(), String> {
        unsafe {
            let handler = ConcreteBlock::new(move |event: *mut Object| -> *mut Object {
                if event.is_null() {
                    return event;
                }

                let send_object_message: unsafe extern "C" fn(*mut Object, Sel) -> *mut Object =
                    mem::transmute(objc_msgSend as unsafe extern "C" fn());
                let event_window = send_object_message(event, sel_registerName(c"window".as_ptr()));
                // 主窗口与浮动助手窗口共用同一连续点击监视：命中哪个窗口就按哪个窗口的
                // 安全区和视图处理，其余窗口的事件直接放行。
                let (target_view, safe_width_bits) =
                    if event_window == MAIN_NATIVE_WINDOW.load(Ordering::Acquire) {
                        (
                            MAIN_NATIVE_VIEW.load(Ordering::Acquire),
                            NATIVE_CONTROL_SAFE_WIDTH_BITS.load(Ordering::Acquire),
                        )
                    } else if event_window == FLOAT_NATIVE_WINDOW.load(Ordering::Acquire) {
                        (
                            FLOAT_NATIVE_VIEW.load(Ordering::Acquire),
                            FLOAT_SAFE_WIDTH_BITS.load(Ordering::Acquire),
                        )
                    } else {
                        return event;
                    };
                if event_window.is_null() {
                    return event;
                }

                let send_integer_message: unsafe extern "C" fn(*mut Object, Sel) -> usize =
                    mem::transmute(objc_msgSend as unsafe extern "C" fn());
                let click_count =
                    send_integer_message(event, sel_registerName(c"clickCount".as_ptr()));
                if target_view.is_null() {
                    return event;
                }
                let location: NSPoint = msg_send![event.cast::<AnyObject>(), locationInWindow];
                let bounds: NSRect = msg_send![target_view.cast::<AnyObject>(), bounds];
                let hit_test = RepeatedClickHitTest {
                    location_x: location.x,
                    location_y: location.y,
                    view_width: bounds.size.width,
                    view_height: bounds.size.height,
                    titlebar_height: f32::from_bits(TITLEBAR_HEIGHT_BITS.load(Ordering::Acquire))
                        as f64,
                    native_control_safe_width: f32::from_bits(safe_width_bits) as f64,
                };
                if !should_intercept_repeated_titlebar_click(click_count, hit_test) {
                    return event;
                }

                let send_event_message: unsafe extern "C" fn(*mut Object, Sel, *mut Object) =
                    mem::transmute(objc_msgSend as unsafe extern "C" fn());
                send_event_message(target_view, sel_registerName(c"mouseDown:".as_ptr()), event);
                // 返回 nil 会取消 NSWindow 后续默认分发，原生标题栏因此无法执行自动缩放。
                ptr::null_mut()
            })
            .copy();

            let event_class = objc_getClass(c"NSEvent".as_ptr());
            if event_class.is_null() {
                return Err("无法读取 AppKit NSEvent 类".to_string());
            }
            let add_monitor: unsafe extern "C" fn(
                *mut Object,
                Sel,
                usize,
                *const Block<(*mut Object,), *mut Object>,
            ) -> *mut Object = mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let monitor = add_monitor(
                event_class.cast_mut().cast::<Object>(),
                sel_registerName(c"addLocalMonitorForEventsMatchingMask:handler:".as_ptr()),
                LEFT_MOUSE_DOWN_EVENT_MASK,
                &*handler,
            );
            if monitor.is_null() {
                return Err("无法安装 Argus 主窗口连续点击事件监视器".to_string());
            }

            // AppKit 持有监视器；保留本地 block 引用至进程结束，避免跨 FFI 生命周期悬空。
            mem::forget(handler);
        }

        Ok(())
    }

    /// 判断是否应绕过 AppKit 的标题栏默认连续点击处理。
    ///
    /// 参数说明：
    /// - `click_count`：AppKit 为当前左键按下事件累计的连续点击次数。
    /// - `hit_test`：事件位置、实时视图尺寸以及标题栏安全区约束。
    ///
    /// 返回值：第二次及后续连续按下返回 `true`，首次按下返回 `false`。
    ///
    /// AppKit 对标题栏连续点击的处理并不只限定为 `clickCount == 2`；如果仅取消第二次
    /// 按下，第三次仍可能重新进入原生窗口缩放逻辑。这里只接管自定义标题栏内部的重复
    /// 点击，正文、原生交通灯和窗口缩放边缘继续由 AppKit 正常处理。
    fn should_intercept_repeated_titlebar_click(
        click_count: usize,
        hit_test: RepeatedClickHitTest,
    ) -> bool {
        if click_count < 2
            || !hit_test.location_x.is_finite()
            || !hit_test.location_y.is_finite()
            || hit_test.view_width <= 0.0
            || hit_test.view_height <= 0.0
        {
            return false;
        }

        let distance_from_top = hit_test.view_height - hit_test.location_y;
        hit_test.location_x >= hit_test.native_control_safe_width
            && hit_test.location_x <= hit_test.view_width - WINDOW_RESIZE_EDGE_INSET
            && distance_from_top >= WINDOW_RESIZE_EDGE_INSET
            && distance_from_top <= hit_test.titlebar_height
    }

    /// 使用当前 AppKit 鼠标按下事件发起原生窗口拖动，保留跨 Space 等系统能力。
    pub(super) fn start_window_drag(window: &Window) {
        let Ok(native_view) = native_view(window) else {
            return;
        };

        unsafe {
            let send_object_message: unsafe extern "C" fn(*mut Object, Sel) -> *mut Object =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let native_window =
                send_object_message(native_view, sel_registerName(c"window".as_ptr()));
            if native_window.is_null() {
                return;
            }
            let application_class = objc_getClass(c"NSApplication".as_ptr());
            if application_class.is_null() {
                return;
            }
            let application = send_object_message(
                application_class.cast_mut().cast::<Object>(),
                sel_registerName(c"sharedApplication".as_ptr()),
            );
            let current_event =
                send_object_message(application, sel_registerName(c"currentEvent".as_ptr()));
            if current_event.is_null() {
                return;
            }
            let send_object_argument_message: unsafe extern "C" fn(*mut Object, Sel, *mut Object) =
                mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send_object_argument_message(
                native_window,
                sel_registerName(c"performWindowDragWithEvent:".as_ptr()),
                current_event,
            );
        }
    }

    /// 从 GPUI 窗口的 raw-window-handle 中取得当前 macOS `NSView` 指针。
    fn native_view(window: &Window) -> Result<*mut Object, String> {
        let window_handle = HasWindowHandle::window_handle(window)
            .map_err(|error| format!("无法取得主窗口原生句柄：{error}"))?;
        match window_handle.as_raw() {
            RawWindowHandle::AppKit(handle) => Ok(handle.ns_view.as_ptr().cast::<Object>()),
            _ => Err("当前窗口不是 macOS AppKit 窗口".to_string()),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{RepeatedClickHitTest, should_intercept_repeated_titlebar_click};

        /// 构造 1330×820 主窗口内指定 GPUI 顶部坐标对应的原生命中参数。
        fn hit_test_at(location_x: f64, distance_from_top: f64) -> RepeatedClickHitTest {
            RepeatedClickHitTest {
                location_x,
                location_y: 820.0 - distance_from_top,
                view_width: 1330.0,
                view_height: 820.0,
                titlebar_height: 40.0,
                native_control_safe_width: 96.0,
            }
        }

        /// 验证标题栏标签区域的三连击及更多连续点击都会绕过 AppKit 默认缩放。
        #[test]
        fn intercepts_every_repeated_titlebar_click() {
            let tab_hit_test = hit_test_at(500.0, 20.0);

            assert!(!should_intercept_repeated_titlebar_click(1, tab_hit_test));
            assert!(should_intercept_repeated_titlebar_click(2, tab_hit_test));
            assert!(should_intercept_repeated_titlebar_click(3, tab_hit_test));
            assert!(should_intercept_repeated_titlebar_click(4, tab_hit_test));
        }

        /// 验证正文、交通灯和窗口缩放边缘不会被自定义标题栏监视器截获。
        #[test]
        fn keeps_non_titlebar_repeated_clicks_in_appkit() {
            assert!(!should_intercept_repeated_titlebar_click(
                2,
                hit_test_at(500.0, 100.0)
            ));
            assert!(!should_intercept_repeated_titlebar_click(
                2,
                hit_test_at(50.0, 20.0)
            ));
            assert!(!should_intercept_repeated_titlebar_click(
                2,
                hit_test_at(500.0, 2.0)
            ));
            assert!(!should_intercept_repeated_titlebar_click(
                2,
                hit_test_at(1328.0, 20.0)
            ));
        }
    }
}
