//! 文件职责：实现压缩 TAR 归档条目枚举适配器。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：处理 tar.gz、tar.bz2、tar.xz 外层解压并复用 TAR 条目枚举逻辑。

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use bzip2::read::BzDecoder;
use flate2::read::GzDecoder;
use xz2::read::XzDecoder;

use crate::loader::archive::adapter::{
    ArchiveAdapter, ArchiveCapabilities, ArchiveEntryConsumer, ArchiveEntryInfo, ArchiveReadSeek,
    ArchiveRootProbe,
};
use crate::loader::archive::detector::ArchiveFormat;
use crate::loader::archive::tar_adapter::{
    list_tar_entries, probe_tar_single_file_root, read_tar_entry_bytes, stream_tar_entry,
};

/// tar.gz 可识别扩展名。
const TAR_GZ_EXTENSIONS: &[&str] = &[".tar.gz", ".tgz"];
/// tar.bz2 可识别扩展名。
const TAR_BZ2_EXTENSIONS: &[&str] = &[".tar.bz2", ".tbz2", ".tbz"];
/// tar.xz 可识别扩展名。
const TAR_XZ_EXTENSIONS: &[&str] = &[".tar.xz", ".txz"];
/// 异常压缩 TAR 实例的空扩展名兜底。
const EMPTY_EXTENSIONS: &[&str] = &[];

/// 压缩 TAR 适配器，按格式选择外层解压器。
#[derive(Debug)]
pub struct CompressedTarArchiveAdapter {
    /// 当前压缩 TAR 的具体外层格式。
    pub format: ArchiveFormat,
}

impl ArchiveAdapter for CompressedTarArchiveAdapter {
    /// 声明压缩 TAR 格式的识别规则和可用能力。
    fn capabilities(&self) -> ArchiveCapabilities {
        let (label, extensions) = match self.format {
            ArchiveFormat::TarGz => ("tar.gz", TAR_GZ_EXTENSIONS),
            ArchiveFormat::TarBz2 => ("tar.bz2", TAR_BZ2_EXTENSIONS),
            ArchiveFormat::TarXz => ("tar.xz", TAR_XZ_EXTENSIONS),
            _ => ("压缩 TAR", EMPTY_EXTENSIONS),
        };

        ArchiveCapabilities {
            format: self.format,
            label,
            extensions,
            supports_header_detection: true,
            supports_listing: true,
            supports_entry_reading: true,
            supports_nested_archives: true,
            supports_passwords: false,
        }
    }

    /// 根据外层压缩编码签名识别压缩 TAR。
    fn matches_header(&self, sample: &[u8]) -> bool {
        match self.format {
            ArchiveFormat::TarGz => sample.starts_with(&[0x1F, 0x8B]),
            ArchiveFormat::TarBz2 => sample.starts_with(b"BZh"),
            ArchiveFormat::TarXz => sample.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]),
            _ => false,
        }
    }

    /// 枚举压缩 TAR 内部条目。
    fn list_entries(&self, path: &Path) -> Result<Vec<ArchiveEntryInfo>> {
        let file = File::open(path)
            .with_context(|| format!("无法打开压缩 TAR 归档：{}", path.display()))?;

        match self.format {
            ArchiveFormat::TarGz => {
                list_tar_entries(GzDecoder::new(file), &path.display().to_string())
            }
            ArchiveFormat::TarBz2 => {
                list_tar_entries(BzDecoder::new(file), &path.display().to_string())
            }
            ArchiveFormat::TarXz => {
                list_tar_entries(XzDecoder::new(file), &path.display().to_string())
            }
            _ => bail!("{} 不是压缩 TAR 格式", self.format.label()),
        }
    }

    /// 从内存压缩 TAR 数据源枚举条目。
    fn list_entries_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        _reader_len: u64,
        source_label: &str,
    ) -> Result<Vec<ArchiveEntryInfo>> {
        list_compressed_tar_entries(reader, self.format, source_label)
    }

    /// 轻量探测压缩 TAR 根层单文件，外层解压后复用 TAR 短路探测。
    fn probe_single_file_root(&self, path: &Path) -> Result<ArchiveRootProbe> {
        let file = File::open(path)
            .with_context(|| format!("无法打开压缩 TAR 归档：{}", path.display()))?;
        probe_compressed_tar_single_file_root(file, self.format, &path.display().to_string())
    }

    /// 从内存压缩 TAR 数据源轻量探测根层单文件。
    fn probe_single_file_root_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        _reader_len: u64,
        source_label: &str,
    ) -> Result<ArchiveRootProbe> {
        probe_compressed_tar_single_file_root(reader, self.format, source_label)
    }

    /// 从本地压缩 TAR 读取指定条目字节。
    fn read_entry_bytes(&self, path: &Path, entry_path: &str) -> Result<Vec<u8>> {
        let file = File::open(path)
            .with_context(|| format!("无法打开压缩 TAR 归档：{}", path.display()))?;
        read_compressed_tar_entry_bytes(file, self.format, entry_path, &path.display().to_string())
    }

    /// 从内存压缩 TAR 读取指定条目字节。
    fn read_entry_bytes_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        _reader_len: u64,
        entry_path: &str,
        source_label: &str,
    ) -> Result<Vec<u8>> {
        read_compressed_tar_entry_bytes(reader, self.format, entry_path, source_label)
    }

    /// 从本地压缩 TAR 流式读取指定条目内容。
    fn stream_entry(
        &self,
        path: &Path,
        entry_path: &str,
        consumer: &mut ArchiveEntryConsumer<'_>,
    ) -> Result<()> {
        let file = File::open(path)
            .with_context(|| format!("无法打开压缩 TAR 归档：{}", path.display()))?;
        stream_compressed_tar_entry(
            file,
            self.format,
            entry_path,
            &path.display().to_string(),
            consumer,
        )
    }

    /// 从内存压缩 TAR 流式读取指定条目内容。
    fn stream_entry_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        _reader_len: u64,
        entry_path: &str,
        source_label: &str,
        consumer: &mut ArchiveEntryConsumer<'_>,
    ) -> Result<()> {
        stream_compressed_tar_entry(reader, self.format, entry_path, source_label, consumer)
    }
}

/// 从任意读取器中短路探测压缩 TAR 根层单文件。
pub fn probe_compressed_tar_single_file_root<R>(
    reader: R,
    format: ArchiveFormat,
    source_label: &str,
) -> Result<ArchiveRootProbe>
where
    R: Read,
{
    match format {
        ArchiveFormat::TarGz => probe_tar_single_file_root(GzDecoder::new(reader), source_label),
        ArchiveFormat::TarBz2 => probe_tar_single_file_root(BzDecoder::new(reader), source_label),
        ArchiveFormat::TarXz => probe_tar_single_file_root(XzDecoder::new(reader), source_label),
        _ => bail!("{} 不是压缩 TAR 格式", format.label()),
    }
}

/// 从任意读取器中枚举压缩 TAR 条目。
///
/// 参数说明：
/// - `reader`：压缩 TAR 数据来源。
/// - `format`：压缩 TAR 外层编码格式。
/// - `source_label`：错误提示中的来源名称。
///
/// 返回值：解压外层后的 TAR 条目列表。
pub fn list_compressed_tar_entries<R>(
    reader: R,
    format: ArchiveFormat,
    source_label: &str,
) -> Result<Vec<ArchiveEntryInfo>>
where
    R: Read,
{
    match format {
        ArchiveFormat::TarGz => list_tar_entries(GzDecoder::new(reader), source_label),
        ArchiveFormat::TarBz2 => list_tar_entries(BzDecoder::new(reader), source_label),
        ArchiveFormat::TarXz => list_tar_entries(XzDecoder::new(reader), source_label),
        _ => bail!("{} 不是压缩 TAR 格式", format.label()),
    }
}

/// 从任意读取器中读取压缩 TAR 指定条目的完整字节。
pub fn read_compressed_tar_entry_bytes<R>(
    reader: R,
    format: ArchiveFormat,
    entry_path: &str,
    source_label: &str,
) -> Result<Vec<u8>>
where
    R: Read,
{
    match format {
        ArchiveFormat::TarGz => {
            read_tar_entry_bytes(GzDecoder::new(reader), entry_path, source_label)
        }
        ArchiveFormat::TarBz2 => {
            read_tar_entry_bytes(BzDecoder::new(reader), entry_path, source_label)
        }
        ArchiveFormat::TarXz => {
            read_tar_entry_bytes(XzDecoder::new(reader), entry_path, source_label)
        }
        _ => bail!("{} 不是压缩 TAR 格式", format.label()),
    }
}

/// 从任意读取器中流式读取压缩 TAR 指定条目的字节。
///
/// 参数说明：
/// - `reader`：压缩 TAR 数据来源。
/// - `format`：压缩 TAR 外层编码格式。
/// - `entry_path`：目标 TAR 条目路径。
/// - `source_label`：错误提示中的来源名称。
/// - `consumer`：接收解压后字节分片的回调。
pub fn stream_compressed_tar_entry<R>(
    reader: R,
    format: ArchiveFormat,
    entry_path: &str,
    source_label: &str,
    consumer: &mut ArchiveEntryConsumer<'_>,
) -> Result<()>
where
    R: Read,
{
    match format {
        ArchiveFormat::TarGz => {
            stream_tar_entry(GzDecoder::new(reader), entry_path, source_label, consumer)
        }
        ArchiveFormat::TarBz2 => {
            stream_tar_entry(BzDecoder::new(reader), entry_path, source_label, consumer)
        }
        ArchiveFormat::TarXz => {
            stream_tar_entry(XzDecoder::new(reader), entry_path, source_label, consumer)
        }
        _ => bail!("{} 不是压缩 TAR 格式", format.label()),
    }
}
