//! 文件职责：提供 Argus 运行时内存资产源。
//! 创建日期：2026-06-09
//! 修改日期：2026-10-28
//! 作者：Argus 开发团队
//! 主要功能：将 icondata 的 Lucide 图标数据和少量界面形状映射为 GPUI 可加载的 SVG 资产。

use crate::ui::components::icon::ArgusIcon;
use gpui::{AssetSource, SharedString};
use std::borrow::Cow;

/// 应用图标资产路径；欢迎页直接展示应用图标。
pub(crate) const APP_ICON_ASSET_PATH: &str = "icons/app-icon.png";
/// 应用图标字节；编译期嵌入，运行时不依赖安装目录中的资源文件。
const APP_ICON_BYTES: &[u8] = include_bytes!("../assets/icons/app-icon.png");

/// Argus 内存资产源，提供 Lucide SVG 图标与应用图标。
#[derive(Debug, Default)]
pub(crate) struct ArgusAssetSource;

impl ArgusAssetSource {
    /// 创建资产源实例，当前不读取文件系统。
    pub(crate) fn new() -> Self {
        Self
    }
}

impl AssetSource for ArgusAssetSource {
    /// 根据路径加载内存中的 SVG 图标或界面形状。
    ///
    /// 参数说明：
    /// - `path`：GPUI SVG 元素请求的资产路径，例如 `icons/search.svg`。
    ///
    /// 返回值：匹配图标时返回完整 SVG 字节；未知路径返回 `None`，不抛出业务异常。
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if path == APP_ICON_ASSET_PATH {
            return Ok(Some(Cow::Borrowed(APP_ICON_BYTES)));
        }

        let Some(icon) = ArgusIcon::from_path(path) else {
            return Ok(None);
        };

        Ok(Some(Cow::Owned(icon.to_svg_string().into_bytes())))
    }

    /// 列出指定目录下的可用图标资产。
    ///
    /// 参数说明：
    /// - `path`：目录路径；当前只支持 `icons`。
    ///
    /// 返回值：可用图标文件名列表；未知目录返回空列表。
    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        if path != "icons" {
            return Ok(Vec::new());
        }

        let mut assets = ArgusIcon::all()
            .iter()
            .map(|icon| SharedString::from(icon.file_name()))
            .collect::<Vec<_>>();
        assets.push(SharedString::from("app-icon.png"));
        Ok(assets)
    }
}

#[cfg(test)]
mod tests {
    use super::{APP_ICON_ASSET_PATH, ArgusAssetSource};
    use gpui::AssetSource;

    /// 验证应用图标通过资产源内嵌加载，未知路径仍返回 None。
    #[test]
    fn loads_embedded_app_icon_and_rejects_unknown_assets() {
        let source = ArgusAssetSource::new();
        let bytes = source
            .load(APP_ICON_ASSET_PATH)
            .expect("加载应用图标不应失败")
            .expect("应用图标应内嵌在二进制中");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "应用图标应为 PNG");
        assert!(
            source
                .load("icons/not-a-real-icon.svg")
                .expect("未知资产应正常返回")
                .is_none()
        );
    }
}
