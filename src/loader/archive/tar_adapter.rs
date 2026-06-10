//! 文件职责：实现 TAR 归档条目枚举适配器。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：打开普通 TAR 文件并枚举目录树所需的条目元信息。

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context as _, Result, bail};

use crate::loader::archive::adapter::{
    ArchiveAdapter, ArchiveCapabilities, ArchiveEntryInfo, ArchiveReadSeek,
};
use crate::loader::archive::detector::ArchiveFormat;
use crate::utils::path::normalize_archive_entry_path;

/// 普通 TAR 适配器，当前只做条目枚举。
#[derive(Debug, Default)]
pub struct TarArchiveAdapter;

impl ArchiveAdapter for TarArchiveAdapter {
    /// 声明 TAR 格式的识别规则和可用能力。
    fn capabilities(&self) -> ArchiveCapabilities {
        ArchiveCapabilities {
            format: ArchiveFormat::Tar,
            label: "TAR",
            extensions: &[".tar"],
            supports_header_detection: true,
            supports_listing: true,
            supports_entry_reading: true,
            supports_nested_archives: true,
            supports_passwords: false,
        }
    }

    /// TAR 的 ustar 魔数位于头部偏移 257 处。
    fn matches_header(&self, sample: &[u8]) -> bool {
        sample.get(257..262) == Some(b"ustar")
    }

    /// 枚举 TAR 条目并转换为统一条目模型。
    fn list_entries(&self, path: &Path) -> Result<Vec<ArchiveEntryInfo>> {
        let file =
            File::open(path).with_context(|| format!("无法打开 TAR 归档：{}", path.display()))?;
        list_tar_entries(file, &path.display().to_string())
    }

    /// 从内存 TAR 数据源枚举条目。
    fn list_entries_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        _reader_len: u64,
        source_label: &str,
    ) -> Result<Vec<ArchiveEntryInfo>> {
        list_tar_entries(reader, source_label)
    }

    /// 从本地 TAR 读取指定条目字节。
    fn read_entry_bytes(&self, path: &Path, entry_path: &str) -> Result<Vec<u8>> {
        let file =
            File::open(path).with_context(|| format!("无法打开 TAR 归档：{}", path.display()))?;
        read_tar_entry_bytes(file, entry_path, &path.display().to_string())
    }

    /// 从内存 TAR 读取指定条目字节。
    fn read_entry_bytes_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        _reader_len: u64,
        entry_path: &str,
        source_label: &str,
    ) -> Result<Vec<u8>> {
        read_tar_entry_bytes(reader, entry_path, source_label)
    }
}

/// 从任意读取器中枚举 TAR 条目，供普通 TAR 和压缩 TAR 复用。
pub fn list_tar_entries<R>(reader: R, source_label: &str) -> Result<Vec<ArchiveEntryInfo>>
where
    R: std::io::Read,
{
    let mut archive = tar::Archive::new(reader);
    let mut entries = Vec::new();

    for entry in archive
        .entries()
        .with_context(|| format!("无法读取 TAR 条目：{source_label}"))?
    {
        let entry = entry.with_context(|| format!("无法解析 TAR 条目：{source_label}"))?;
        let entry_path = normalize_archive_entry_path(&entry.path()?.to_string_lossy());
        if entry_path.is_empty() {
            continue;
        }

        let label = entry_path
            .rsplit('/')
            .next()
            .unwrap_or(entry_path.as_str())
            .to_string();
        entries.push(ArchiveEntryInfo {
            path: entry_path,
            label,
            is_dir: entry.header().entry_type().is_dir(),
            size: Some(entry.size()),
        });
    }

    Ok(entries)
}

/// 从任意读取器中读取 TAR 指定条目的完整字节。
///
/// 参数说明：
/// - `reader`：TAR 数据来源。
/// - `entry_path`：目标条目路径，统一使用 `/` 分隔。
/// - `source_label`：错误提示中的来源名称。
///
/// 返回值：目标条目的原始字节；目录条目不会返回内容。
pub fn read_tar_entry_bytes<R>(reader: R, entry_path: &str, source_label: &str) -> Result<Vec<u8>>
where
    R: std::io::Read,
{
    let normalized_entry_path = normalize_archive_entry_path(entry_path);
    let mut archive = tar::Archive::new(reader);

    for entry in archive
        .entries()
        .with_context(|| format!("无法读取 TAR 条目：{source_label}"))?
    {
        let mut entry = entry.with_context(|| format!("无法解析 TAR 条目：{source_label}"))?;
        let current_path = normalize_archive_entry_path(&entry.path()?.to_string_lossy());
        if current_path != normalized_entry_path {
            continue;
        }
        if entry.header().entry_type().is_dir() {
            bail!("TAR 条目是目录，无法读取内容：{normalized_entry_path}");
        }

        let mut bytes = Vec::with_capacity(entry.size().try_into().unwrap_or(0));
        entry
            .read_to_end(&mut bytes)
            .with_context(|| format!("无法读取 TAR 条目内容：{normalized_entry_path}"))?;
        return Ok(bytes);
    }

    bail!("未找到 TAR 条目：{normalized_entry_path}")
}
