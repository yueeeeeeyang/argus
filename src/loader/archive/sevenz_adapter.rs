//! 文件职责：实现 7Z 压缩包条目枚举适配器。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：使用 sevenz-rust 枚举 7Z 条目元信息，不读取日志正文。

use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use sevenz_rust::{Error as SevenzError, Password, SevenZReader};

use crate::loader::archive::adapter::{
    ArchiveAdapter, ArchiveCapabilities, ArchiveEntryConsumer, ArchiveEntryInfo, ArchiveReadSeek,
    ArchiveRootProbe, ArchiveRootProbeState,
};
use crate::loader::archive::detector::ArchiveFormat;
use crate::utils::path::normalize_archive_entry_path;

/// 7Z 适配器，当前只做条目枚举；加密压缩包会由底层库返回错误。
#[derive(Debug, Default)]
pub struct SevenzArchiveAdapter;

impl ArchiveAdapter for SevenzArchiveAdapter {
    /// 声明 7Z 格式的识别规则和可用能力。
    fn capabilities(&self) -> ArchiveCapabilities {
        ArchiveCapabilities {
            format: ArchiveFormat::SevenZ,
            label: "7Z",
            extensions: &[".7z"],
            supports_header_detection: true,
            supports_listing: true,
            supports_entry_reading: true,
            supports_nested_archives: true,
            supports_passwords: false,
        }
    }

    /// 7Z 文件头固定为 `37 7A BC AF 27 1C`。
    fn matches_header(&self, sample: &[u8]) -> bool {
        sample.starts_with(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C])
    }

    /// 枚举 7Z 条目并转换为统一条目模型。
    fn list_entries(&self, path: &Path) -> Result<Vec<ArchiveEntryInfo>> {
        let file =
            File::open(path).with_context(|| format!("无法打开 7Z 压缩包：{}", path.display()))?;
        let reader_len = file
            .metadata()
            .with_context(|| format!("无法读取 7Z 文件大小：{}", path.display()))?
            .len();
        list_sevenz_entries_from_reader(file, reader_len, &path.display().to_string())
    }

    /// 从内存 7Z 数据源枚举条目。
    fn list_entries_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        reader_len: u64,
        source_label: &str,
    ) -> Result<Vec<ArchiveEntryInfo>> {
        list_sevenz_entries_from_reader(reader, reader_len, source_label)
    }

    /// 轻量探测 7Z 根层单文件；只读取 7Z 元数据，不解压正文。
    fn probe_single_file_root(&self, path: &Path) -> Result<ArchiveRootProbe> {
        let file =
            File::open(path).with_context(|| format!("无法打开 7Z 压缩包：{}", path.display()))?;
        let reader_len = file
            .metadata()
            .with_context(|| format!("无法读取 7Z 文件大小：{}", path.display()))?
            .len();
        probe_sevenz_single_file_root_from_reader(file, reader_len, &path.display().to_string())
    }

    /// 从内存 7Z 数据源轻量探测根层单文件。
    fn probe_single_file_root_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        reader_len: u64,
        source_label: &str,
    ) -> Result<ArchiveRootProbe> {
        probe_sevenz_single_file_root_from_reader(reader, reader_len, source_label)
    }

    /// 从本地 7Z 读取指定条目字节。
    fn read_entry_bytes(&self, path: &Path, entry_path: &str) -> Result<Vec<u8>> {
        let file =
            File::open(path).with_context(|| format!("无法打开 7Z 压缩包：{}", path.display()))?;
        let reader_len = file
            .metadata()
            .with_context(|| format!("无法读取 7Z 文件大小：{}", path.display()))?
            .len();
        read_sevenz_entry_bytes_from_reader(
            file,
            reader_len,
            entry_path,
            &path.display().to_string(),
        )
    }

    /// 从内存 7Z 读取指定条目字节。
    fn read_entry_bytes_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        reader_len: u64,
        entry_path: &str,
        source_label: &str,
    ) -> Result<Vec<u8>> {
        read_sevenz_entry_bytes_from_reader(reader, reader_len, entry_path, source_label)
    }

    /// 从本地 7Z 流式读取指定条目内容。
    fn stream_entry(
        &self,
        path: &Path,
        entry_path: &str,
        consumer: &mut ArchiveEntryConsumer<'_>,
    ) -> Result<()> {
        let file =
            File::open(path).with_context(|| format!("无法打开 7Z 压缩包：{}", path.display()))?;
        let reader_len = file
            .metadata()
            .with_context(|| format!("无法读取 7Z 文件大小：{}", path.display()))?
            .len();
        stream_sevenz_entry_from_reader(
            file,
            reader_len,
            entry_path,
            &path.display().to_string(),
            consumer,
        )
    }

    /// 从内存 7Z 流式读取指定条目内容。
    fn stream_entry_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        reader_len: u64,
        entry_path: &str,
        source_label: &str,
        consumer: &mut ArchiveEntryConsumer<'_>,
    ) -> Result<()> {
        stream_sevenz_entry_from_reader(reader, reader_len, entry_path, source_label, consumer)
    }
}

/// 从任意 7Z 数据源短路探测根层单文件。
pub fn probe_sevenz_single_file_root_from_reader<R>(
    reader: R,
    reader_len: u64,
    source_label: &str,
) -> Result<ArchiveRootProbe>
where
    R: Read + Seek,
{
    let reader = SevenZReader::new(reader, reader_len, Password::empty())
        .with_context(|| format!("无法解析 7Z 压缩包：{source_label}"))?;
    let mut state = ArchiveRootProbeState::default();

    for entry in reader.archive().files.iter() {
        let entry_path = normalize_archive_entry_path(entry.name());
        if entry_path.is_empty() {
            continue;
        }

        let label = entry_path
            .rsplit('/')
            .next()
            .unwrap_or(entry_path.as_str())
            .to_string();
        let entry_info = ArchiveEntryInfo {
            path: entry_path,
            label,
            is_dir: entry.is_directory(),
            size: Some(entry.size()),
        };
        if !state.observe(entry_info) {
            break;
        }
    }

    Ok(state.finish())
}

/// 从任意可读可 seek 的输入枚举 7Z 条目。
pub fn list_sevenz_entries_from_reader<R>(
    reader: R,
    reader_len: u64,
    source_label: &str,
) -> Result<Vec<ArchiveEntryInfo>>
where
    R: Read + Seek,
{
    let reader = SevenZReader::new(reader, reader_len, Password::empty())
        .with_context(|| format!("无法解析 7Z 压缩包：{source_label}"))?;
    let mut entries = Vec::new();

    for entry in reader.archive().files.iter() {
        let entry_path = normalize_archive_entry_path(entry.name());
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
            is_dir: entry.is_directory(),
            size: Some(entry.size()),
        });
    }

    Ok(entries)
}

/// 从任意 7Z 数据源读取指定条目的完整字节。
pub fn read_sevenz_entry_bytes_from_reader<R>(
    reader: R,
    reader_len: u64,
    entry_path: &str,
    source_label: &str,
) -> Result<Vec<u8>>
where
    R: Read + Seek,
{
    let mut bytes = Vec::new();
    stream_sevenz_entry_from_reader(reader, reader_len, entry_path, source_label, &mut |chunk| {
        bytes.extend_from_slice(chunk);
        Ok(())
    })?;
    Ok(bytes)
}

/// 从任意 7Z 数据源流式读取指定条目。
pub fn stream_sevenz_entry_from_reader<R>(
    reader: R,
    reader_len: u64,
    entry_path: &str,
    source_label: &str,
    consumer: &mut ArchiveEntryConsumer<'_>,
) -> Result<()>
where
    R: Read + Seek,
{
    let normalized_entry_path = normalize_archive_entry_path(entry_path);
    let mut reader = SevenZReader::new(reader, reader_len, Password::empty())
        .with_context(|| format!("无法解析 7Z 压缩包：{source_label}"))?;
    let mut found_entry = false;
    let mut found_directory = false;
    let mut callback_error = None;
    let mut buffer = [0_u8; 64 * 1024];

    reader
        .for_each_entries(|entry, entry_reader| {
            let current_path = normalize_archive_entry_path(entry.name());
            if current_path != normalized_entry_path {
                return Ok(true);
            }
            found_entry = true;
            if entry.is_directory() {
                found_directory = true;
                return Ok(false);
            }

            loop {
                let read_count = entry_reader.read(&mut buffer).map_err(SevenzError::io)?;
                if read_count == 0 {
                    break;
                }
                if let Err(error) = consumer(&buffer[..read_count]) {
                    callback_error = Some(error);
                    return Ok(false);
                }
            }
            Ok(false)
        })
        .with_context(|| format!("无法读取 7Z 条目：{normalized_entry_path}"))?;

    if let Some(error) = callback_error {
        return Err(error).with_context(|| format!("无法消费 7Z 条目：{normalized_entry_path}"));
    }

    if found_directory {
        bail!("7Z 条目是目录，无法读取内容：{normalized_entry_path}");
    }

    if !found_entry {
        bail!("未找到 7Z 条目：{normalized_entry_path}");
    }

    Ok(())
}
