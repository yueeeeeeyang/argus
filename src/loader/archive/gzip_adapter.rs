//! 文件职责：实现普通 GZIP 单文件压缩适配器。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：把 `.gz` 单文件压缩包展开为一个虚拟日志条目，并支持嵌套压缩链路流式读取。

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use flate2::read::GzDecoder;

use crate::loader::archive::adapter::{
    ArchiveAdapter, ArchiveCapabilities, ArchiveEntryConsumer, ArchiveEntryInfo, ArchiveReadSeek,
    ArchiveRootProbe,
};
use crate::loader::archive::detector::ArchiveFormat;
use crate::utils::path::normalize_archive_entry_path;

/// GZIP 单文件压缩包适配器；不同于 tar.gz，它只有一个虚拟文件条目。
#[derive(Debug, Default)]
pub(crate) struct GzipArchiveAdapter;

impl ArchiveAdapter for GzipArchiveAdapter {
    /// 声明普通 GZIP 的识别规则和可用能力。
    fn capabilities(&self) -> ArchiveCapabilities {
        ArchiveCapabilities {
            format: ArchiveFormat::Gzip,
            label: "GZIP",
            extensions: &[".gz"],
            supports_header_detection: true,
            supports_listing: true,
            supports_entry_reading: true,
            supports_nested_archives: true,
        }
    }

    /// GZIP 文件头固定以 `1F 8B` 开始。
    fn matches_header(&self, sample: &[u8]) -> bool {
        sample.starts_with(&[0x1F, 0x8B])
    }

    /// 枚举本地 GZIP 的唯一虚拟文件条目。
    fn list_entries(&self, path: &Path, _password: Option<&str>) -> Result<Vec<ArchiveEntryInfo>> {
        let file_name = path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .unwrap_or("content.gz");
        Ok(single_gzip_entry(file_name))
    }

    /// 枚举内存 GZIP 的唯一虚拟文件条目。
    fn list_entries_from_reader(
        &self,
        _reader: &mut dyn ArchiveReadSeek,
        _reader_len: u64,
        source_label: &str,
        _password: Option<&str>,
    ) -> Result<Vec<ArchiveEntryInfo>> {
        Ok(single_gzip_entry(source_label))
    }

    /// GZIP 天然只有一个虚拟文件条目，可直接返回单文件探测结果。
    fn probe_single_file_root(
        &self,
        path: &Path,
        _password: Option<&str>,
    ) -> Result<ArchiveRootProbe> {
        let file_name = path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .unwrap_or("content.gz");
        Ok(ArchiveRootProbe::SingleFile(
            single_gzip_entry(file_name).remove(0),
        ))
    }

    /// 从内存 GZIP 数据源探测唯一虚拟文件条目。
    fn probe_single_file_root_from_reader(
        &self,
        _reader: &mut dyn ArchiveReadSeek,
        _reader_len: u64,
        source_label: &str,
        _password: Option<&str>,
    ) -> Result<ArchiveRootProbe> {
        Ok(ArchiveRootProbe::SingleFile(
            single_gzip_entry(source_label).remove(0),
        ))
    }

    /// 从本地 GZIP 读取虚拟文件完整字节。
    fn read_entry_bytes(
        &self,
        path: &Path,
        entry_path: &str,
        _password: Option<&str>,
    ) -> Result<Vec<u8>> {
        let file =
            File::open(path).with_context(|| format!("无法打开 GZIP 文件：{}", path.display()))?;
        read_gzip_entry_bytes(file, entry_path, &path.display().to_string())
    }

    /// 从内存 GZIP 读取虚拟文件完整字节。
    fn read_entry_bytes_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        _reader_len: u64,
        entry_path: &str,
        source_label: &str,
        _password: Option<&str>,
    ) -> Result<Vec<u8>> {
        read_gzip_entry_bytes(reader, entry_path, source_label)
    }

    /// 从本地 GZIP 流式读取虚拟文件内容。
    fn stream_entry(
        &self,
        path: &Path,
        entry_path: &str,
        _password: Option<&str>,
        consumer: &mut ArchiveEntryConsumer<'_>,
    ) -> Result<()> {
        let file =
            File::open(path).with_context(|| format!("无法打开 GZIP 文件：{}", path.display()))?;
        stream_gzip_entry(file, entry_path, &path.display().to_string(), consumer)
    }

    /// 从内存 GZIP 流式读取虚拟文件内容。
    fn stream_entry_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        _reader_len: u64,
        entry_path: &str,
        source_label: &str,
        _password: Option<&str>,
        consumer: &mut ArchiveEntryConsumer<'_>,
    ) -> Result<()> {
        stream_gzip_entry(reader, entry_path, source_label, consumer)
    }
}

/// 生成 GZIP 的唯一虚拟文件条目，条目名称为压缩文件名去掉 `.gz` 后的结果。
fn single_gzip_entry(source_label: &str) -> Vec<ArchiveEntryInfo> {
    let label = gzip_payload_label(source_label);
    vec![ArchiveEntryInfo {
        path: label,
        is_dir: false,
        size: None,
    }]
}

/// 读取 GZIP 虚拟条目的完整字节；供嵌套压缩包继续向下解析时使用。
fn read_gzip_entry_bytes<R>(reader: R, entry_path: &str, source_label: &str) -> Result<Vec<u8>>
where
    R: Read,
{
    let mut bytes = Vec::new();
    stream_gzip_entry(reader, entry_path, source_label, &mut |chunk| {
        bytes.extend_from_slice(chunk);
        Ok(())
    })?;
    Ok(bytes)
}

/// 流式解压 GZIP 虚拟条目，避免日志正文读取时生成临时文件。
fn stream_gzip_entry<R>(
    reader: R,
    entry_path: &str,
    source_label: &str,
    consumer: &mut ArchiveEntryConsumer<'_>,
) -> Result<()>
where
    R: Read,
{
    let expected_entry_path = gzip_payload_label(source_label);
    let normalized_entry_path = normalize_archive_entry_path(entry_path);
    if normalized_entry_path != expected_entry_path {
        bail!("GZIP 只有一个虚拟条目：{expected_entry_path}，无法读取 {normalized_entry_path}");
    }

    let mut decoder = GzDecoder::new(reader);
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let bytes_read = decoder
            .read(&mut buffer)
            .with_context(|| format!("GZIP 解压失败：{source_label}"))?;
        if bytes_read == 0 {
            break;
        }
        consumer(&buffer[..bytes_read])?;
    }

    Ok(())
}

/// 从本地路径或 `outer.zip!/inner.log.gz` 形式的虚拟路径推导解压后的文件名。
fn gzip_payload_label(source_label: &str) -> String {
    let after_container = source_label
        .rsplit("!/")
        .next()
        .unwrap_or(source_label)
        .replace('\\', "/");
    let file_name = after_container
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("content.gz");
    strip_gzip_extension(file_name)
}

/// 去除末尾 `.gz` 扩展名；极端空文件名时回退为 `content`。
fn strip_gzip_extension(file_name: &str) -> String {
    let lowercase = file_name.to_ascii_lowercase();
    let stripped = if lowercase.ends_with(".gz") && file_name.len() > 3 {
        &file_name[..file_name.len() - 3]
    } else {
        file_name
    };

    if stripped.is_empty() {
        "content".to_string()
    } else {
        stripped.to_string()
    }
}
