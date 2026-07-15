//! 文件职责：提供居中提示文案组件。
//! 创建日期：2026-07-03
//! 修改日期：2026-07-03
//! 作者：Argus 开发团队
//! 主要功能：渲染居中的次要文案，用于空状态、二进制/错误提示等；长文本自动截断避免溢出窗口。

use gpui::{AnyElement, IntoElement, div, prelude::*, px, rgb};

use crate::theme::AppTheme;

/// 居中提示最大宽度，超过则截断，避免长错误文案溢出窗口。
const CENTERED_MESSAGE_MAX_WIDTH: f32 = 560.0;
/// 居中提示字号。
const CENTERED_MESSAGE_FONT_SIZE: f32 = 13.0;

/// 渲染居中次要文案。
///
/// 参数说明：
/// - `message`：提示文案。
/// - `theme`：主题令牌。
/// - `fill`：`true` 时占满父容器（`size_full`），`false` 时仅占剩余弹性空间（`flex_1`）。
///
/// 返回值：居中文案元素；长文案在最大宽度内截断，不会横向溢出。
pub(crate) fn render_centered_message(message: &str, theme: &AppTheme, fill: bool) -> AnyElement {
    let mut container = div().flex().items_center().justify_center();
    container = if fill {
        container.size_full()
    } else {
        container.flex_1()
    };
    container
        .px_6()
        .text_size(px(CENTERED_MESSAGE_FONT_SIZE))
        .text_color(rgb(theme.foreground_muted))
        .child(
            div()
                .max_w(px(CENTERED_MESSAGE_MAX_WIDTH))
                .truncate()
                .child(message.to_string()),
        )
        .into_any_element()
}
