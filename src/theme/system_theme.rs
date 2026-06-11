//! 文件职责：提供系统窗口外观到 Argus 主题模式的适配能力。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：把 GPUI 平台外观统一映射为主题系统可识别的 dark/light 模式。

use gpui::WindowAppearance;

/// 根据 GPUI 窗口外观返回主题管理器使用的模式标识。
///
/// 参数说明：
/// - `appearance`：GPUI 从平台窗口读取到的当前外观。
///
/// 返回值：`"dark"` 或 `"light"`，可直接传给 `ThemeManager::theme_for_mode`。
pub fn theme_mode_for_window_appearance(appearance: WindowAppearance) -> &'static str {
    match appearance {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => "dark",
        WindowAppearance::Light | WindowAppearance::VibrantLight => "light",
    }
}

/// 返回系统外观在设置提示中使用的中文文案。
///
/// 参数说明：
/// - `appearance`：当前平台窗口外观。
///
/// 返回值：简短中文标签，便于提示“当前跟随到深色/浅色”。
pub fn label_for_window_appearance(appearance: WindowAppearance) -> &'static str {
    match appearance {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => "深色",
        WindowAppearance::Light | WindowAppearance::VibrantLight => "浅色",
    }
}
