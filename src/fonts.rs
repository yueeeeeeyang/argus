//! 文件职责：集中管理 Argus 内置字体注册与界面、日志字体名称。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：注册界面字体和日志阅读字体资源，并向 UI 层提供统一字体族常量。

use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};

use gpui::App;

/// Argus 主界面和来源树使用的字体族名称。
pub(crate) const ARGUS_UI_FONT_FAMILY: &str = "Microsoft YaHei Mono";
/// Argus 日志正文使用的等宽字体族名称。
pub(crate) const ARGUS_LOG_FONT_FAMILY: &str = "JetBrains Mono";

/// 仓库内置界面字体文件路径；字体文件需由具备授权的贡献者放入该位置。
const MICROSOFT_YAHEI_MONO_FONT_PATH: &str = "assets/fonts/MicrosoftYaHeiMono.ttf";
/// 仓库内置日志字体文件路径；日志阅读区固定使用该字体资源。
const JETBRAINS_MONO_REGULAR_FONT_PATH: &str = "assets/fonts/JetBrainsMono-Regular.ttf";

/// 在 GPUI 文本系统中注册 Argus 内置字体。
///
/// 参数说明：
/// - `cx`：GPUI 应用上下文，用于访问全局文本系统。
///
/// 返回值：注册成功或字体文件不存在时返回 `Ok(())`；字体文件存在但加载失败时返回错误。
///
/// 说明：
/// - 界面字体缺失时仍设置字体族名称，系统若已安装同名字体会直接使用，否则由平台字体回退兜底。
/// - 日志字体优先读取 `assets/fonts/JetBrainsMono-Regular.ttf`，确保日志阅读区使用稳定的等宽字体。
pub(crate) fn register_argus_fonts(cx: &mut App) -> anyhow::Result<()> {
    let mut fonts = Vec::new();
    for path in embedded_font_paths() {
        if let Some(font_bytes) = load_font_bytes(&path)? {
            fonts.push(Cow::Owned(font_bytes));
        }
    }

    if !fonts.is_empty() {
        cx.text_system().add_fonts(fonts)?;
    }
    Ok(())
}

/// 读取指定仓库内置字体文件。
fn load_font_bytes(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    if !path.exists() {
        return Ok(None);
    }

    fs::read(path)
        .map(Some)
        .map_err(|error| anyhow::anyhow!("无法读取内置字体文件 {}：{}", path.display(), error))
}

/// 返回仓库内置字体文件绝对路径列表。
fn embedded_font_paths() -> Vec<PathBuf> {
    vec![embedded_ui_font_path(), embedded_log_font_path()]
}

/// 返回仓库内置界面字体文件绝对路径。
fn embedded_ui_font_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(MICROSOFT_YAHEI_MONO_FONT_PATH)
}

/// 返回仓库内置日志字体文件绝对路径。
fn embedded_log_font_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(JETBRAINS_MONO_REGULAR_FONT_PATH)
}

#[cfg(test)]
mod tests {
    use super::{
        ARGUS_LOG_FONT_FAMILY, ARGUS_UI_FONT_FAMILY, embedded_log_font_path, embedded_ui_font_path,
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

    /// 验证内置界面字体路径位于仓库字体资源目录下。
    #[test]
    fn embedded_ui_font_path_points_to_assets_fonts() {
        assert!(embedded_ui_font_path().ends_with("assets/fonts/MicrosoftYaHeiMono.ttf"));
    }

    /// 验证内置日志字体路径位于仓库字体资源目录下。
    #[test]
    fn embedded_log_font_path_points_to_assets_fonts() {
        assert!(embedded_log_font_path().ends_with("assets/fonts/JetBrainsMono-Regular.ttf"));
    }
}
