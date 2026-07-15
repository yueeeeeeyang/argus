//! 文件职责：实现 7Z 压缩包条目枚举适配器。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：使用 sevenz-rust 枚举 7Z 条目元信息，不读取日志正文。

use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use sevenz_rust::{Error as SevenzError, Password, SevenZMethod, SevenZReader};

use crate::loader::archive::adapter::{
    ArchiveAdapter, ArchiveCapabilities, ArchiveEntryConsumer, ArchiveEntryInfo, ArchiveReadSeek,
    ArchiveRootProbe, ArchiveRootProbeState,
};
use crate::loader::archive::detector::ArchiveFormat;
use crate::loader::archive::password::ArchivePasswordError;
use crate::utils::path::normalize_archive_entry_path;

/// 7Z 适配器，当前只做条目枚举；加密压缩包会由底层库返回错误。
#[derive(Debug, Default)]
pub(crate) struct SevenzArchiveAdapter;

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
        }
    }

    /// 7Z 文件头固定为 `37 7A BC AF 27 1C`。
    fn matches_header(&self, sample: &[u8]) -> bool {
        sample.starts_with(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C])
    }

    /// 枚举 7Z 条目并转换为统一条目模型。
    fn list_entries(&self, path: &Path, password: Option<&str>) -> Result<Vec<ArchiveEntryInfo>> {
        let file =
            File::open(path).with_context(|| format!("无法打开 7Z 压缩包：{}", path.display()))?;
        let reader_len = file
            .metadata()
            .with_context(|| format!("无法读取 7Z 文件大小：{}", path.display()))?
            .len();
        list_sevenz_entries_from_reader(file, reader_len, &path.display().to_string(), password)
    }

    /// 从内存 7Z 数据源枚举条目。
    fn list_entries_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        reader_len: u64,
        source_label: &str,
        password: Option<&str>,
    ) -> Result<Vec<ArchiveEntryInfo>> {
        list_sevenz_entries_from_reader(reader, reader_len, source_label, password)
    }

    /// 轻量探测 7Z 根层单文件；只读取 7Z 元数据，不解压正文。
    fn probe_single_file_root(
        &self,
        path: &Path,
        password: Option<&str>,
    ) -> Result<ArchiveRootProbe> {
        let file =
            File::open(path).with_context(|| format!("无法打开 7Z 压缩包：{}", path.display()))?;
        let reader_len = file
            .metadata()
            .with_context(|| format!("无法读取 7Z 文件大小：{}", path.display()))?
            .len();
        probe_sevenz_single_file_root_from_reader(
            file,
            reader_len,
            &path.display().to_string(),
            password,
        )
    }

    /// 从内存 7Z 数据源轻量探测根层单文件。
    fn probe_single_file_root_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        reader_len: u64,
        source_label: &str,
        password: Option<&str>,
    ) -> Result<ArchiveRootProbe> {
        probe_sevenz_single_file_root_from_reader(reader, reader_len, source_label, password)
    }

    /// 从本地 7Z 读取指定条目字节。
    fn read_entry_bytes(
        &self,
        path: &Path,
        entry_path: &str,
        password: Option<&str>,
    ) -> Result<Vec<u8>> {
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
            password,
        )
    }

    /// 从内存 7Z 读取指定条目字节。
    fn read_entry_bytes_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        reader_len: u64,
        entry_path: &str,
        source_label: &str,
        password: Option<&str>,
    ) -> Result<Vec<u8>> {
        read_sevenz_entry_bytes_from_reader(reader, reader_len, entry_path, source_label, password)
    }

    /// 从本地 7Z 流式读取指定条目内容。
    fn stream_entry(
        &self,
        path: &Path,
        entry_path: &str,
        password: Option<&str>,
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
            password,
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
        password: Option<&str>,
        consumer: &mut ArchiveEntryConsumer<'_>,
    ) -> Result<()> {
        stream_sevenz_entry_from_reader(
            reader,
            reader_len,
            entry_path,
            source_label,
            password,
            consumer,
        )
    }
}

/// 从任意 7Z 数据源短路探测根层单文件。
pub(crate) fn probe_sevenz_single_file_root_from_reader<R>(
    reader: R,
    reader_len: u64,
    source_label: &str,
    password: Option<&str>,
) -> Result<ArchiveRootProbe>
where
    R: Read + Seek,
{
    let reader = open_sevenz_reader(reader, reader_len, password, source_label)?;
    ensure_sevenz_password_if_encrypted(reader.archive(), password, source_label)?;
    let mut state = ArchiveRootProbeState::default();

    for entry in reader.archive().files.iter() {
        let entry_path = normalize_archive_entry_path(entry.name());
        if entry_path.is_empty() {
            continue;
        }

        let entry_info = ArchiveEntryInfo {
            path: entry_path,
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
pub(crate) fn list_sevenz_entries_from_reader<R>(
    reader: R,
    reader_len: u64,
    source_label: &str,
    password: Option<&str>,
) -> Result<Vec<ArchiveEntryInfo>>
where
    R: Read + Seek,
{
    let reader = open_sevenz_reader(reader, reader_len, password, source_label)?;
    ensure_sevenz_password_if_encrypted(reader.archive(), password, source_label)?;
    let mut entries = Vec::new();

    for entry in reader.archive().files.iter() {
        let entry_path = normalize_archive_entry_path(entry.name());
        if entry_path.is_empty() {
            continue;
        }

        entries.push(ArchiveEntryInfo {
            path: entry_path,
            is_dir: entry.is_directory(),
            size: Some(entry.size()),
        });
    }

    Ok(entries)
}

/// 从任意 7Z 数据源读取指定条目的完整字节。
pub(crate) fn read_sevenz_entry_bytes_from_reader<R>(
    reader: R,
    reader_len: u64,
    entry_path: &str,
    source_label: &str,
    password: Option<&str>,
) -> Result<Vec<u8>>
where
    R: Read + Seek,
{
    let mut bytes = Vec::new();
    stream_sevenz_entry_from_reader(
        reader,
        reader_len,
        entry_path,
        source_label,
        password,
        &mut |chunk| {
            bytes.extend_from_slice(chunk);
            Ok(())
        },
    )?;
    Ok(bytes)
}

/// 从任意 7Z 数据源流式读取指定条目。
pub(crate) fn stream_sevenz_entry_from_reader<R>(
    reader: R,
    reader_len: u64,
    entry_path: &str,
    source_label: &str,
    password: Option<&str>,
    consumer: &mut ArchiveEntryConsumer<'_>,
) -> Result<()>
where
    R: Read + Seek,
{
    let normalized_entry_path = normalize_archive_entry_path(entry_path);
    let mut reader = open_sevenz_reader(reader, reader_len, password, source_label)?;
    ensure_sevenz_password_if_encrypted(reader.archive(), password, source_label)?;
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
        .map_err(|error| map_sevenz_error(error, source_label))
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

/// 使用可选密码打开 7Z 读取器，并转换底层密码错误。
fn open_sevenz_reader<R>(
    reader: R,
    reader_len: u64,
    password: Option<&str>,
    source_label: &str,
) -> Result<SevenZReader<R>>
where
    R: Read + Seek,
{
    SevenZReader::new(reader, reader_len, sevenz_password(password))
        .map_err(|error| map_sevenz_error(error, source_label))
        .with_context(|| format!("无法解析 7Z 压缩包：{source_label}"))
}

/// 将普通字符串密码转换为 sevenz-rust 使用的 UTF-16LE 密码格式。
fn sevenz_password(password: Option<&str>) -> Password {
    password.map(Password::from).unwrap_or_else(Password::empty)
}

/// 如果 7Z 文件包含 AES 加密方法，则没有密码时立即返回需要密码，避免展示不可打开的子级。
fn ensure_sevenz_password_if_encrypted(
    archive: &sevenz_rust::Archive,
    password: Option<&str>,
    source_label: &str,
) -> Result<()> {
    if password.is_some() || !archive_contains_encrypted_folder(archive) {
        return Ok(());
    }

    Err(ArchivePasswordError::required(source_label).into())
}

/// 判断 7Z 归档中是否存在 AES256-SHA256 加密 coder。
fn archive_contains_encrypted_folder(archive: &sevenz_rust::Archive) -> bool {
    archive.folders.iter().any(|folder| {
        folder
            .coders
            .iter()
            .any(|coder| coder.decompression_method_id() == SevenZMethod::ID_AES256SHA256)
    })
}

/// 将 sevenz-rust 的密码错误转换为 Argus 统一错误。
fn map_sevenz_error(error: SevenzError, source_label: &str) -> anyhow::Error {
    match error {
        SevenzError::PasswordRequired => ArchivePasswordError::required(source_label).into(),
        SevenzError::MaybeBadPassword(_) | SevenzError::ChecksumVerificationFailed => {
            ArchivePasswordError::invalid(source_label).into()
        }
        SevenzError::Unsupported(message) => {
            let detail = message.to_string();
            if detail.to_ascii_lowercase().contains("aes")
                || detail.to_ascii_lowercase().contains("password")
            {
                ArchivePasswordError::unsupported(source_label, detail).into()
            } else {
                SevenzError::Unsupported(message).into()
            }
        }
        SevenzError::UnsupportedCompressionMethod(message) => {
            if message.to_ascii_lowercase().contains("aes")
                || message.to_ascii_lowercase().contains("password")
            {
                ArchivePasswordError::unsupported(source_label, message).into()
            } else {
                SevenzError::UnsupportedCompressionMethod(message).into()
            }
        }
        other => other.into(),
    }
}
