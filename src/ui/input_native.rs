//! 文件职责：为自绘输入框创建 GPUI 原生文本输入桥接配置。
//! 创建日期：2026-06-16
//! 修改日期：2026-06-16
//! 作者：Argus 开发团队
//! 主要功能：把输入法提交的文本编辑结果转发到主应用的统一输入状态处理入口。

use gpui::{App, Entity, FocusHandle, Window};

use crate::app::{AppTextInputTarget, ArgusApp};
use crate::infra::text_selection::NativeTextEdit;
use crate::ui::components::input::NativeInput;

/// 创建绑定到指定应用输入目标的原生文本输入桥。
///
/// 参数说明：
/// - `app_handle`：主应用实体。
/// - `target`：要写回的业务输入框。
/// - `focus_handle`：该输入框的真实 GPUI 焦点句柄。
///
/// 返回值：可放入通用 `Input` 组件的原生输入配置。
pub fn app_native_input(
    app_handle: Entity<ArgusApp>,
    target: AppTextInputTarget,
    focus_handle: FocusHandle,
) -> NativeInput {
    NativeInput::new(
        focus_handle,
        move |edit: NativeTextEdit, _window: &mut Window, cx: &mut App| {
            app_handle.update(cx, |app, app_cx| {
                app.apply_native_text_input_edit_with_context(target, edit, app_cx);
                app_cx.notify();
            });
        },
    )
}
