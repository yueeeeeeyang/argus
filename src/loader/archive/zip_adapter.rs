//! 文件职责：实现 ZIP 压缩包条目枚举适配器。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：打开本地 ZIP 文件并枚举目录树所需的条目元信息。

use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use zip::ZipArchive;

use crate::loader::archive::adapter::{
    ArchiveAdapter, ArchiveCapabilities, ArchiveEntryConsumer, ArchiveEntryInfo, ArchiveReadSeek,
    ArchiveRootProbe, ArchiveRootProbeState,
};
use crate::loader::archive::detector::ArchiveFormat;
use crate::utils::path::normalize_archive_entry_path;

/// ZIP 适配器，当前只做条目枚举，不读取日志正文。
#[derive(Debug, Default)]
pub struct ZipArchiveAdapter;

impl ArchiveAdapter for ZipArchiveAdapter {
    /// 声明 ZIP 格式的识别规则和可用能力。
    fn capabilities(&self) -> ArchiveCapabilities {
        ArchiveCapabilities {
            format: ArchiveFormat::Zip,
            label: "ZIP",
            extensions: &[".zip"],
            supports_header_detection: true,
            supports_listing: true,
            supports_entry_reading: true,
            supports_nested_archives: true,
            supports_passwords: false,
        }
    }

    /// ZIP 文件头包含本地文件头、空归档和跨段描述符等常见签名。
    fn matches_header(&self, sample: &[u8]) -> bool {
        sample.starts_with(b"PK\x03\x04")
            || sample.starts_with(b"PK\x05\x06")
            || sample.starts_with(b"PK\x07\x08")
    }

    /// 枚举 ZIP 条目并转换为统一条目模型。
    fn list_entries(&self, path: &Path) -> Result<Vec<ArchiveEntryInfo>> {
        let file =
            File::open(path).with_context(|| format!("无法打开 ZIP 压缩包：{}", path.display()))?;
        list_zip_entries_from_reader(file, &path.display().to_string())
    }

    /// 从内存 ZIP 数据源枚举条目。
    fn list_entries_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        _reader_len: u64,
        source_label: &str,
    ) -> Result<Vec<ArchiveEntryInfo>> {
        list_zip_entries_from_reader(reader, source_label)
    }

    /// 轻量探测 ZIP 根层单文件，发现第二个根层项或目录后立即停止。
    fn probe_single_file_root(&self, path: &Path) -> Result<ArchiveRootProbe> {
        let file =
            File::open(path).with_context(|| format!("无法打开 ZIP 压缩包：{}", path.display()))?;
        probe_zip_single_file_root_from_reader(file, &path.display().to_string())
    }

    /// 从内存 ZIP 数据源轻量探测根层单文件。
    fn probe_single_file_root_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        _reader_len: u64,
        source_label: &str,
    ) -> Result<ArchiveRootProbe> {
        probe_zip_single_file_root_from_reader(reader, source_label)
    }

    /// 从本地 ZIP 读取指定条目字节。
    fn read_entry_bytes(&self, path: &Path, entry_path: &str) -> Result<Vec<u8>> {
        read_zip_entry_bytes(path, entry_path)
    }

    /// 从内存 ZIP 读取指定条目字节。
    fn read_entry_bytes_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        _reader_len: u64,
        entry_path: &str,
        source_label: &str,
    ) -> Result<Vec<u8>> {
        read_zip_entry_bytes_from_reader(reader, entry_path, source_label)
    }

    /// 从本地 ZIP 流式读取指定条目内容。
    fn stream_entry(
        &self,
        path: &Path,
        entry_path: &str,
        consumer: &mut ArchiveEntryConsumer<'_>,
    ) -> Result<()> {
        let file =
            File::open(path).with_context(|| format!("无法打开 ZIP 压缩包：{}", path.display()))?;
        stream_zip_entry_from_reader(file, entry_path, &path.display().to_string(), consumer)
    }

    /// 从内存 ZIP 流式读取指定条目内容。
    fn stream_entry_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        _reader_len: u64,
        entry_path: &str,
        source_label: &str,
        consumer: &mut ArchiveEntryConsumer<'_>,
    ) -> Result<()> {
        stream_zip_entry_from_reader(reader, entry_path, source_label, consumer)
    }
}

/// 从任意可读可 seek 的 ZIP 输入中短路探测根层单文件。
pub fn probe_zip_single_file_root_from_reader<R>(
    reader: R,
    source_label: &str,
) -> Result<ArchiveRootProbe>
where
    R: Read + Seek,
{
    let mut archive =
        ZipArchive::new(reader).with_context(|| format!("无法解析 ZIP 压缩包：{source_label}"))?;
    let mut state = ArchiveRootProbeState::default();

    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .with_context(|| format!("无法读取 ZIP 第 {index} 个条目：{source_label}"))?;
        let entry_path = normalize_archive_entry_path(file.name());
        if entry_path.is_empty() {
            continue;
        }

        let label = entry_path
            .rsplit('/')
            .next()
            .unwrap_or(entry_path.as_str())
            .to_string();
        let entry = ArchiveEntryInfo {
            path: entry_path,
            label,
            is_dir: file.is_dir(),
            size: Some(file.size()),
        };
        if !state.observe(entry) {
            break;
        }
    }

    Ok(state.finish())
}

/// 从任意可读可 seek 的输入枚举 ZIP 条目。
///
/// 参数说明：
/// - `reader`：ZIP 数据来源，可为本地文件或内存 Cursor。
/// - `source_label`：错误提示中的来源名称。
///
/// 返回值：压缩包内条目列表；不读取日志正文。
pub fn list_zip_entries_from_reader<R>(
    reader: R,
    source_label: &str,
) -> Result<Vec<ArchiveEntryInfo>>
where
    R: Read + Seek,
{
    let mut archive =
        ZipArchive::new(reader).with_context(|| format!("无法解析 ZIP 压缩包：{source_label}"))?;
    let mut entries = Vec::new();

    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .with_context(|| format!("无法读取 ZIP 第 {index} 个条目：{source_label}"))?;
        let entry_path = normalize_archive_entry_path(file.name());
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
            is_dir: file.is_dir(),
            size: Some(file.size()),
        });
    }

    Ok(entries)
}

/// 从本地 ZIP 压缩包读取指定条目的完整字节。
///
/// 返回值：条目内容字节；用于内嵌 ZIP 的内存枚举，不落盘解压。
pub fn read_zip_entry_bytes(path: &Path, entry_path: &str) -> Result<Vec<u8>> {
    let file =
        File::open(path).with_context(|| format!("无法打开 ZIP 压缩包：{}", path.display()))?;
    read_zip_entry_bytes_from_reader(file, entry_path, &path.display().to_string())
}

/// 从任意 ZIP 数据源读取指定条目的完整字节。
///
/// 参数说明：
/// - `reader`：ZIP 数据来源，可为本地文件或内存 Cursor。
/// - `entry_path`：需要读取的内部条目路径。
/// - `source_label`：错误提示中的来源名称。
pub fn read_zip_entry_bytes_from_reader<R>(
    reader: R,
    entry_path: &str,
    source_label: &str,
) -> Result<Vec<u8>>
where
    R: Read + Seek,
{
    let mut bytes = Vec::new();
    stream_zip_entry_from_reader(reader, entry_path, source_label, &mut |chunk| {
        bytes.extend_from_slice(chunk);
        Ok(())
    })?;
    Ok(bytes)
}

/// 从任意 ZIP 数据源流式读取指定条目。
///
/// 参数说明：
/// - `reader`：ZIP 数据来源，可为本地文件或内存 Cursor。
/// - `entry_path`：需要读取的内部条目路径。
/// - `source_label`：错误提示中的来源名称。
/// - `consumer`：接收解压后字节分片的回调。
pub fn stream_zip_entry_from_reader<R>(
    reader: R,
    entry_path: &str,
    source_label: &str,
    consumer: &mut ArchiveEntryConsumer<'_>,
) -> Result<()>
where
    R: Read + Seek,
{
    let normalized_entry_path = normalize_archive_entry_path(entry_path);
    let mut archive =
        ZipArchive::new(reader).with_context(|| format!("无法解析 ZIP 压缩包：{source_label}"))?;
    let mut buffer = [0_u8; 64 * 1024];

    // ZIP 中央目录支持按名称直接定位条目；大量 Runtime 日志逐个读取时可避免每次线性扫描全部条目。
    if let Ok(mut file) = archive.by_name(&normalized_entry_path) {
        if file.is_dir() {
            bail!("ZIP 条目是目录，无法读取内容：{normalized_entry_path}");
        }

        loop {
            let read_count = file.read(&mut buffer).with_context(|| {
                format!("无法读取 ZIP 条目内容 {normalized_entry_path}：{source_label}")
            })?;
            if read_count == 0 {
                return Ok(());
            }
            consumer(&buffer[..read_count])?;
        }
    }

    // 部分异常压缩包可能使用反斜杠或不规范路径名；保留旧的归一化扫描作为兼容回退。
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .with_context(|| format!("无法读取 ZIP 第 {index} 个条目：{source_label}"))?;
        let current_path = normalize_archive_entry_path(file.name());
        if current_path != normalized_entry_path {
            continue;
        }
        if file.is_dir() {
            bail!("ZIP 条目是目录，无法读取内容：{normalized_entry_path}");
        }

        loop {
            let read_count = file.read(&mut buffer).with_context(|| {
                format!("无法读取 ZIP 条目内容 {normalized_entry_path}：{source_label}")
            })?;
            if read_count == 0 {
                break;
            }
            consumer(&buffer[..read_count])?;
        }
        return Ok(());
    }

    anyhow::bail!("无法读取 ZIP 条目 {normalized_entry_path}：{source_label}")
}
