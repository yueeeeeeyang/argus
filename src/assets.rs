//! 文件职责：提供 Argus 运行时内存资产源。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：将 icondata 的 Lucide 图标数据和少量界面形状映射为 GPUI 可加载的 SVG 资产。

use crate::ui::components::icon::ArgusIcon;
use gpui::{AssetSource, SharedString};
use std::borrow::Cow;

/// Argus 内存资产源，当前只负责提供 Lucide SVG 图标。
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

        Ok(ArgusIcon::all()
            .iter()
            .map(|icon| SharedString::from(icon.file_name()))
            .collect())
    }
}
