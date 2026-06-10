//! 文件职责：提供 Argus 运行时内存资产源。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：将 icondata 的 Lucide 图标数据和少量界面形状映射为 GPUI 可加载的 SVG 资产。

use crate::ui::components::icon::ArgusIcon;
use gpui::{AssetSource, SharedString};
use std::borrow::Cow;

/// 左侧激活标签凹弧连接件 SVG，使用 currentColor 继承标题栏颜色。
const TAB_CONNECTOR_LEFT_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="6" height="6" viewBox="0 0 6 6"><path d="M0 0H6C6 3.314 3.314 6 0 6V0Z" fill="currentColor"/></svg>"#;
/// 右侧激活标签凹弧连接件 SVG，使用 currentColor 继承标题栏颜色。
const TAB_CONNECTOR_RIGHT_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="6" height="6" viewBox="0 0 6 6"><path d="M0 0C0 3.314 2.686 6 6 6V0H0Z" fill="currentColor"/></svg>"#;

/// Argus 内存资产源，当前只负责提供 Lucide SVG 图标。
#[derive(Debug, Default)]
pub struct ArgusAssetSource;

impl ArgusAssetSource {
    /// 创建资产源实例，当前不读取文件系统。
    pub fn new() -> Self {
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
        match path {
            "chrome/tab-connector-left.svg" => {
                return Ok(Some(Cow::Borrowed(TAB_CONNECTOR_LEFT_SVG.as_bytes())));
            }
            "chrome/tab-connector-right.svg" => {
                return Ok(Some(Cow::Borrowed(TAB_CONNECTOR_RIGHT_SVG.as_bytes())));
            }
            _ => {}
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

        Ok(ArgusIcon::all()
            .iter()
            .map(|icon| SharedString::from(icon.file_name()))
            .collect())
    }
}
