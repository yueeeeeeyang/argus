//! 文件职责：处理暂不支持压缩格式的统一错误提示。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：为未来识别但尚未接入的压缩格式生成明确、可展示的能力缺失错误。

use std::path::Path;

use anyhow::{Result, bail};

use crate::loader::archive::adapter::{
    ArchiveAdapter, ArchiveCapabilities, ArchiveEntryInfo, ArchiveReadSeek,
};
use crate::loader::archive::detector::ArchiveFormat;

/// 不支持压缩格式适配器，始终返回用户可理解的错误。
#[derive(Debug)]
pub(crate) struct UnsupportedArchiveAdapter {
    /// 被识别出的压缩格式。
    pub format: ArchiveFormat,
}

impl ArchiveAdapter for UnsupportedArchiveAdapter {
    /// 声明不支持格式的能力缺失状态。
    fn capabilities(&self) -> ArchiveCapabilities {
        ArchiveCapabilities {
            format: self.format,
            label: "不支持格式",
            extensions: &[],
            supports_header_detection: false,
            supports_listing: false,
            supports_entry_reading: false,
            supports_nested_archives: false,
            supports_passwords: false,
        }
    }

    /// 返回不支持错误，调用方会将其展示到状态栏或错误节点中。
    fn list_entries(&self, path: &Path, _password: Option<&str>) -> Result<Vec<ArchiveEntryInfo>> {
        bail!("{} 暂不支持展开：{}", self.format.label(), path.display())
    }

    /// 不支持格式无法从内存数据源枚举条目。
    fn list_entries_from_reader(
        &self,
        _reader: &mut dyn ArchiveReadSeek,
        _reader_len: u64,
        source_label: &str,
        _password: Option<&str>,
    ) -> Result<Vec<ArchiveEntryInfo>> {
        bail!("{} 暂不支持展开：{source_label}", self.format.label())
    }

    /// 不支持格式无法读取本地压缩包条目。
    fn read_entry_bytes(
        &self,
        path: &Path,
        entry_path: &str,
        _password: Option<&str>,
    ) -> Result<Vec<u8>> {
        bail!(
            "{} 暂不支持读取条目：{}!/{entry_path}",
            self.format.label(),
            path.display()
        )
    }

    /// 不支持格式无法读取内存压缩包条目。
    fn read_entry_bytes_from_reader(
        &self,
        _reader: &mut dyn ArchiveReadSeek,
        _reader_len: u64,
        entry_path: &str,
        source_label: &str,
        _password: Option<&str>,
    ) -> Result<Vec<u8>> {
        bail!(
            "{} 暂不支持读取条目：{source_label}!/{entry_path}",
            self.format.label()
        )
    }
}
