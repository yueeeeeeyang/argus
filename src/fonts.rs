//! 文件职责：集中管理 Argus 内置字体注册与界面、日志字体名称。
//! 创建日期：2026-06-11
//! 修改日期：2026-10-29
//! 作者：Argus 开发团队
//! 主要功能：注册界面字体和日志阅读字体资源，并向 UI 层提供统一字体族常量。

use std::borrow::Cow;

use gpui::App;

/// Argus 主界面和来源树使用的字体族名称。
pub(crate) const ARGUS_UI_FONT_FAMILY: &str = "Microsoft YaHei Mono";
/// Argus 日志正文使用的等宽字体族名称。
pub(crate) const ARGUS_LOG_FONT_FAMILY: &str = "JetBrains Mono";

/// 日志字体随可执行文件分发，安装位置与构建机源码目录无关。
const JETBRAINS_MONO_REGULAR_FONT: &[u8] =
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");

/// 在 GPUI 文本系统中注册 Argus 内置字体。
///
/// 参数说明：
/// - `cx`：GPUI 应用上下文，用于访问全局文本系统。
///
/// 返回值：注册成功返回 `Ok(())`；内嵌字体无法加载时返回错误。
///
/// 说明：
/// - 界面字体由操作系统提供；系统未安装 Microsoft YaHei Mono 时使用平台字体回退。
/// - 日志字体直接使用编译期嵌入的字节，不依赖当前目录或安装路径。
pub(crate) fn register_argus_fonts(cx: &mut App) -> anyhow::Result<()> {
    cx.text_system()
        .add_fonts(vec![Cow::Borrowed(JETBRAINS_MONO_REGULAR_FONT)])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ARGUS_LOG_FONT_FAMILY, ARGUS_UI_FONT_FAMILY, JETBRAINS_MONO_REGULAR_FONT,
        register_argus_fonts,
    };

    /// 验证 UI 字体族名称固定为设计指定值。
    #[test]
    fn ui_font_family_is_microsoft_yahei_mono() {
        assert_eq!(ARGUS_UI_FONT_FAMILY, "Microsoft YaHei Mono");
    }

    /// 验证日志字体族名称固定为 JetBrains Mono。
    #[test]
    fn log_font_family_is_jetbrains_mono() {
        assert_eq!(ARGUS_LOG_FONT_FAMILY, "JetBrains Mono");
    }

    /// GPUI 测试平台使用空字体系统；独立解析嵌入字节验证字体有效性及阅读所需的等宽度量。
    #[test]
    fn embedded_font_contains_expected_family_and_monospace_glyphs() {
        let face = ttf_parser::Face::parse(JETBRAINS_MONO_REGULAR_FONT, 0)
            .expect("内嵌字体必须是合法的 TrueType 字体");
        assert!(face.names().into_iter().any(|name| {
            name.name_id == ttf_parser::name_id::FAMILY
                && name.to_string().as_deref() == Some(ARGUS_LOG_FONT_FAMILY)
        }));
        assert!(face.is_monospaced());
        let advances = ['i', 'W', '0', ' '].map(|character| {
            let glyph = face
                .glyph_index(character)
                .expect("日志字体必须包含 ASCII 字形");
            face.glyph_hor_advance(glyph).expect("字形必须包含宽度")
        });
        assert!(advances[0] > 0);
        assert!(advances.iter().all(|advance| *advance == advances[0]));
    }

    /// 注册入口直接接收内嵌字节，无须从构建机源码目录读取字体。
    #[gpui::test]
    fn embedded_log_font_registers_without_runtime_file_access(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            register_argus_fonts(cx).expect("内嵌字体应能注册");
        });
    }
}
