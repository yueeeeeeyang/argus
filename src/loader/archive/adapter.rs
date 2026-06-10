//! 文件职责：定义压缩包统一适配器抽象。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：为 ZIP、TAR、压缩 TAR 和 7Z 等格式提供统一条目枚举模型。

use std::path::Path;

use anyhow::Result;

use crate::loader::archive::detector::ArchiveFormat;

/// 压缩包条目枚举结果；只保存结构信息，不读取日志正文内容。
#[derive(Clone, Debug)]
pub struct ArchiveEntryInfo {
    /// 压缩包内规范化路径，统一使用 `/` 分隔。
    pub path: String,
    /// 条目名称，用于来源树显示。
    pub label: String,
    /// 是否为目录条目。
    pub is_dir: bool,
    /// 条目未压缩大小；部分格式可能无法提供。
    pub size: Option<u64>,
}

/// 压缩包适配器统一接口，当前阶段仅要求枚举条目。
pub trait ArchiveAdapter {
    /// 枚举压缩包条目。
    ///
    /// 参数说明：
    /// - `path`：本地压缩包路径。
    ///
    /// 返回值：压缩包内条目列表；不执行正文读取或解压到磁盘。
    fn list_entries(&self, path: &Path) -> Result<Vec<ArchiveEntryInfo>>;
}

/// 根据压缩格式枚举压缩包条目。
pub fn list_archive_entries(path: &Path, format: ArchiveFormat) -> Result<Vec<ArchiveEntryInfo>> {
    match format {
        ArchiveFormat::Zip => {
            crate::loader::archive::zip_adapter::ZipArchiveAdapter.list_entries(path)
        }
        ArchiveFormat::Tar => {
            crate::loader::archive::tar_adapter::TarArchiveAdapter.list_entries(path)
        }
        ArchiveFormat::TarGz | ArchiveFormat::TarBz2 | ArchiveFormat::TarXz => {
            crate::loader::archive::compressed_tar::CompressedTarArchiveAdapter { format }
                .list_entries(path)
        }
        ArchiveFormat::SevenZ => {
            crate::loader::archive::sevenz_adapter::SevenzArchiveAdapter.list_entries(path)
        }
        ArchiveFormat::Rar => {
            crate::loader::archive::unsupported::UnsupportedArchiveAdapter { format }
                .list_entries(path)
        }
    }
}
