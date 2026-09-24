//! 文件职责：提供独立浮动窗口的原生帧读写能力。
//! 创建日期：2026-09-24
//! 修改日期：2026-09-24
//! 作者：Argus 开发团队
//! 主要功能：在 macOS 上读取窗口与所在屏幕的全局帧，并把窗口移动缩放到指定全局帧；其余平台回退为不可用。

use gpui::{Bounds, Pixels, Window};

/// 窗口与其所在屏幕的帧快照；坐标均为全局左上角原点（与 CGDisplayBounds 一致，y 向下）。
#[derive(Clone, Copy, Debug)]
pub(crate) struct WindowFrameSnapshot {
    /// 窗口当前帧。
    pub(crate) window: Bounds<Pixels>,
    /// 窗口所在屏幕的帧。
    pub(crate) screen: Bounds<Pixels>,
}

/// 读取窗口当前帧与所在屏幕帧；macOS 读取原生 NSWindow.frame/screen 并翻转坐标，
/// 其余平台返回 `None`，调用方据此回退为内嵌布局。
#[cfg(target_os = "macos")]
pub(crate) fn window_frame_snapshot(window: &Window) -> Option<WindowFrameSnapshot> {
    macos::window_frame_snapshot(window)
}

/// 非 macOS 平台暂不支持读取原生窗口帧，调用方回退为内嵌布局。
#[cfg(not(target_os = "macos"))]
pub(crate) fn window_frame_snapshot(_window: &Window) -> Option<WindowFrameSnapshot> {
    None
}

/// 把窗口移动并缩放到指定全局帧；macOS 用 `setFrame:display:animate:` 立即生效，
/// 其余平台返回 `false`，调用方据此回退为内嵌布局。
#[cfg(target_os = "macos")]
pub(crate) fn set_window_frame(window: &Window, frame: Bounds<Pixels>) -> bool {
    macos::set_window_frame(window, frame)
}

/// 非 macOS 平台无法移动原生窗口，调用方回退为内嵌布局。
#[cfg(not(target_os = "macos"))]
pub(crate) fn set_window_frame(_window: &Window, _frame: Bounds<Pixels>) -> bool {
    false
}

/// 返回 macOS 原生 `NSWindow` 的不透明指针（仅用于父子窗口关联）；非 macOS 返回 `None`。
#[cfg(target_os = "macos")]
pub(crate) fn native_window_id(window: &Window) -> Option<usize> {
    macos::native_window(window).map(|pointer| pointer as usize)
}

/// 非 macOS 平台没有父子窗口概念，返回 `None`。
#[cfg(not(target_os = "macos"))]
pub(crate) fn native_window_id(_window: &Window) -> Option<usize> {
    None
}

/// 把子窗口挂到父窗口下：macOS 子窗口随父窗口原生移动、隐藏与跨屏，
/// 拖动跟随由 AppKit 完成、零延迟；返回是否挂载成功，非 macOS 返回 `false`。
#[cfg(target_os = "macos")]
pub(crate) fn attach_child_window_native(parent_id: usize, child: &Window) -> bool {
    macos::attach_child_window_native(parent_id, child)
}

/// 非 macOS 平台无法建立父子窗口关联，调用方继续走帧同步跟随。
#[cfg(not(target_os = "macos"))]
pub(crate) fn attach_child_window_native(_parent_id: usize, _child: &Window) -> bool {
    false
}

#[cfg(target_os = "macos")]
mod macos {
    use gpui::{Bounds, Pixels, Window, point, px, size};
    use objc2::{
        msg_send,
        runtime::{AnyClass, AnyObject, Bool},
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::WindowFrameSnapshot;

    /// 读取窗口与所在屏幕的全局帧（原点为主屏左上角，y 向下）。
    pub(super) fn window_frame_snapshot(window: &Window) -> Option<WindowFrameSnapshot> {
        let native_window = native_window(window)?;
        unsafe {
            let window_frame: NSRect = msg_send![native_window, frame];
            let screen: *mut AnyObject = msg_send![native_window, screen];
            if screen.is_null() {
                return None;
            }
            let screen_frame: NSRect = msg_send![screen, frame];
            let primary_height = primary_screen_height()?;
            Some(WindowFrameSnapshot {
                window: cocoa_rect_to_global(window_frame, primary_height),
                screen: cocoa_rect_to_global(screen_frame, primary_height),
            })
        }
    }

    /// 把窗口移动并缩放到指定全局帧；立即生效不使用动画。
    pub(super) fn set_window_frame(window: &Window, frame: Bounds<Pixels>) -> bool {
        let Some(native_window) = native_window(window) else {
            return false;
        };
        let Some(primary_height) = primary_screen_height() else {
            return false;
        };
        let rect = global_rect_to_cocoa(frame, primary_height);
        unsafe {
            let _: () = msg_send![
                native_window,
                setFrame: rect,
                display: Bool::YES,
                animate: Bool::NO
            ];
        }
        true
    }

    /// 把子窗口挂到父窗口下（`NSWindowAbove` 排序）：子窗口随父窗口原生移动与隐藏。
    pub(super) fn attach_child_window_native(parent_id: usize, child: &Window) -> bool {
        let parent = parent_id as *mut AnyObject;
        if parent.is_null() {
            return false;
        }
        let Some(child_window) = native_window(child) else {
            return false;
        };
        // NSWindowAbove = 1：助手窗口始终排在主窗口之上，符合侧栏跟随语义。
        unsafe {
            let _: () = msg_send![parent, addChildWindow: child_window, ordered: 1_usize];
        }
        true
    }

    /// Cocoa 全局坐标（原点为主屏左下角，y 向上）转全局左上角坐标（y 向下）。
    fn cocoa_rect_to_global(rect: NSRect, primary_height: f64) -> Bounds<Pixels> {
        Bounds::new(
            point(
                px(rect.origin.x as f32),
                px((primary_height - rect.origin.y - rect.size.height) as f32),
            ),
            size(px(rect.size.width as f32), px(rect.size.height as f32)),
        )
    }

    /// 全局左上角坐标（y 向下）转 Cocoa 全局坐标。
    fn global_rect_to_cocoa(frame: Bounds<Pixels>, primary_height: f64) -> NSRect {
        NSRect::new(
            NSPoint::new(
                f32::from(frame.origin.x) as f64,
                primary_height
                    - f32::from(frame.origin.y) as f64
                    - f32::from(frame.size.height) as f64,
            ),
            NSSize::new(
                f32::from(frame.size.width) as f64,
                f32::from(frame.size.height) as f64,
            ),
        )
    }

    /// 读取主屏高度（点）；`NSScreen.screens` 第一项即主屏。
    fn primary_screen_height() -> Option<f64> {
        unsafe {
            let screens_class = AnyClass::get(c"NSScreen")?;
            let screens: *mut AnyObject = msg_send![screens_class, screens];
            if screens.is_null() {
                return None;
            }
            let primary: *mut AnyObject = msg_send![screens, firstObject];
            if primary.is_null() {
                return None;
            }
            let frame: NSRect = msg_send![primary, frame];
            (frame.size.height > 0.0).then_some(frame.size.height)
        }
    }

    /// 从 GPUI 窗口的 raw-window-handle 中取得当前 macOS `NSWindow` 指针。
    pub(super) fn native_window(window: &Window) -> Option<*mut AnyObject> {
        let window_handle = HasWindowHandle::window_handle(window).ok()?;
        match window_handle.as_raw() {
            RawWindowHandle::AppKit(handle) => {
                let native_view = handle.ns_view.as_ptr().cast::<AnyObject>();
                let native_window: *mut AnyObject = unsafe { msg_send![native_view, window] };
                (!native_window.is_null()).then_some(native_window)
            }
            _ => None,
        }
    }
}
