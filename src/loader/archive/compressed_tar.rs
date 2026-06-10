//! 文件职责：实现压缩 TAR 归档条目枚举适配器。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：处理 tar.gz、tar.bz2、tar.xz 外层解压并复用 TAR 条目枚举逻辑。

use std::fs::File;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use bzip2::read::BzDecoder;
use flate2::read::GzDecoder;
use xz2::read::XzDecoder;

use crate::loader::archive::adapter::{ArchiveAdapter, ArchiveEntryInfo};
use crate::loader::archive::detector::ArchiveFormat;
use crate::loader::archive::tar_adapter::list_tar_entries;

/// 压缩 TAR 适配器，按格式选择外层解压器。
#[derive(Debug)]
pub struct CompressedTarArchiveAdapter {
    /// 当前压缩 TAR 的具体外层格式。
    pub format: ArchiveFormat,
}

impl ArchiveAdapter for CompressedTarArchiveAdapter {
    /// 枚举压缩 TAR 内部条目。
    fn list_entries(&self, path: &Path) -> Result<Vec<ArchiveEntryInfo>> {
        let file = File::open(path)
            .with_context(|| format!("无法打开压缩 TAR 归档：{}", path.display()))?;

        match self.format {
            ArchiveFormat::TarGz => list_tar_entries(GzDecoder::new(file), path),
            ArchiveFormat::TarBz2 => list_tar_entries(BzDecoder::new(file), path),
            ArchiveFormat::TarXz => list_tar_entries(XzDecoder::new(file), path),
            _ => bail!("{} 不是压缩 TAR 格式", self.format.label()),
        }
    }
}
