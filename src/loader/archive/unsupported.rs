//! 文件职责：处理暂不支持压缩格式的统一错误提示。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：为 RAR 等非 MVP 格式生成明确、可展示的能力缺失错误。

use std::path::Path;

use anyhow::{Result, bail};

use crate::loader::archive::adapter::{ArchiveAdapter, ArchiveEntryInfo};
use crate::loader::archive::detector::ArchiveFormat;

/// 不支持压缩格式适配器，始终返回用户可理解的错误。
#[derive(Debug)]
pub struct UnsupportedArchiveAdapter {
    /// 被识别出的压缩格式。
    pub format: ArchiveFormat,
}

impl ArchiveAdapter for UnsupportedArchiveAdapter {
    /// 返回不支持错误，调用方会将其展示到状态栏或错误节点中。
    fn list_entries(&self, path: &Path) -> Result<Vec<ArchiveEntryInfo>> {
        bail!("{} 暂不支持展开：{}", self.format.label(), path.display())
    }
}
