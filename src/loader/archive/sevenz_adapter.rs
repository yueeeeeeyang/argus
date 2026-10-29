//! 文件职责：实现 7Z 压缩包条目枚举适配器。
//! 创建日期：2026-06-09
//! 修改日期：2026-10-29
//! 作者：Argus 开发团队
//! 主要功能：使用 sevenz-rust 枚举 7Z 条目元信息，批量、按条目读取或流式输出日志内容。

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use sevenz_rust::{Error as SevenzError, Password, SevenZMethod, SevenZReader};
use tokio_util::sync::CancellationToken;

use crate::loader::archive::adapter::{
    ArchiveAdapter, ArchiveCapabilities, ArchiveEntryConsumer, ArchiveEntryInfo, ArchiveEntryTarget,
};
use crate::loader::archive::detector::ArchiveFormat;
use crate::loader::archive::password::ArchivePasswordError;
use crate::utils::path::normalize_archive_entry_path;

/// 7Z 适配器，支持条目枚举、批量提取和按条目读取；加密压缩包由底层库校验密码。
#[derive(Debug, Default)]
pub(crate) struct SevenzArchiveAdapter;

impl ArchiveAdapter for SevenzArchiveAdapter {
    /// 7Z 固实块必须顺序解码，批量提取可以避免每个条目重复解压前面的数据。
    fn supports_batch_extraction(&self) -> bool {
        true
    }

    /// 一次扫描提取多个本地 7Z 条目。
    fn extract_entries_to_paths(
        &self,
        path: &Path,
        targets: &[ArchiveEntryTarget],
        password: Option<&str>,
        max_log_bytes: u64,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        let file =
            File::open(path).with_context(|| format!("无法打开 7Z 压缩包：{}", path.display()))?;
        let reader_len = file
            .metadata()
            .with_context(|| format!("无法读取 7Z 文件大小：{}", path.display()))?
            .len();
        extract_sevenz_entries_to_paths(
            file,
            reader_len,
            &path.display().to_string(),
            password,
            targets,
            max_log_bytes,
            cancellation,
        )
    }

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
}

/// 从任意 7Z 数据源一次顺序扫描提取多个条目。
///
/// 固实 7Z 的条目共享解码流，必须按归档顺序消费；未选中的条目会被丢弃，
/// 保证整块只解码一次而不是每个目标条目都从块头重解开。
pub(crate) fn extract_sevenz_entries_to_paths<R>(
    reader: R,
    reader_len: u64,
    source_label: &str,
    password: Option<&str>,
    targets: &[ArchiveEntryTarget],
    max_log_bytes: u64,
    cancellation: &CancellationToken,
) -> Result<()>
where
    R: Read + Seek,
{
    if targets.is_empty() {
        return Ok(());
    }
    if cancellation.is_cancelled() {
        bail!("已取消归档提取");
    }

    let mut remaining = HashMap::with_capacity(targets.len());
    for target in targets {
        let entry_path = normalize_archive_entry_path(&target.entry_path);
        if remaining
            .insert(entry_path.clone(), target.target.as_path())
            .is_some()
        {
            bail!("批量提取条目重复：{entry_path}");
        }
    }

    let mut reader = open_sevenz_reader(reader, reader_len, password, source_label)?;
    ensure_sevenz_password_if_encrypted(reader.archive(), password, source_label)?;
    let mut buffer = [0_u8; 64 * 1024];
    let mut callback_error = None;

    reader
        .for_each_entries(|entry, entry_reader| {
            if cancellation.is_cancelled() {
                callback_error = Some(anyhow::anyhow!("已取消归档提取"));
                return Ok(false);
            }

            let current_path = normalize_archive_entry_path(entry.name());
            let Some(target) = remaining.remove(current_path.as_str()) else {
                // 固实块中未选中的条目也必须消费，否则后续目标条目会从错误偏移读取。
                loop {
                    if cancellation.is_cancelled() {
                        callback_error = Some(anyhow::anyhow!("已取消归档提取"));
                        return Ok(false);
                    }
                    let read_count = entry_reader.read(&mut buffer).map_err(SevenzError::io)?;
                    if read_count == 0 {
                        break;
                    }
                }
                return Ok(!remaining.is_empty());
            };

            if entry.is_directory() {
                callback_error = Some(anyhow::anyhow!(
                    "7Z 条目是目录，无法读取内容：{current_path}"
                ));
                return Ok(false);
            }
            if entry.size() > max_log_bytes {
                callback_error = Some(anyhow::anyhow!("7Z 条目超过最大日志大小：{current_path}"));
                return Ok(false);
            }
            if let Some(parent) = target.parent()
                && let Err(error) = std::fs::create_dir_all(parent)
            {
                callback_error = Some(
                    anyhow::Error::new(error)
                        .context(format!("无法创建物化目录：{}", parent.display())),
                );
                return Ok(false);
            }
            let mut output = match File::create_new(target) {
                Ok(output) => output,
                Err(error) => {
                    callback_error = Some(
                        anyhow::Error::new(error)
                            .context(format!("无法创建物化文件：{}", target.display())),
                    );
                    return Ok(false);
                }
            };

            let mut written = 0_u64;
            loop {
                if cancellation.is_cancelled() {
                    callback_error = Some(anyhow::anyhow!("已取消归档提取"));
                    return Ok(false);
                }
                let read_count = entry_reader.read(&mut buffer).map_err(SevenzError::io)?;
                if read_count == 0 {
                    break;
                }
                written = written.saturating_add(read_count as u64);
                if written > max_log_bytes {
                    callback_error =
                        Some(anyhow::anyhow!("7Z 条目超过最大日志大小：{current_path}"));
                    return Ok(false);
                }
                if let Err(error) = output.write_all(&buffer[..read_count]) {
                    callback_error = Some(
                        anyhow::Error::new(error)
                            .context(format!("无法写入物化文件：{}", target.display())),
                    );
                    return Ok(false);
                }
            }
            if let Err(error) = output.flush() {
                callback_error = Some(
                    anyhow::Error::new(error)
                        .context(format!("无法写入物化文件：{}", target.display())),
                );
                return Ok(false);
            }

            // 全部目标已写出后立即结束，不再解码后续固实数据。
            Ok(!remaining.is_empty())
        })
        .map_err(|error| map_sevenz_error(error, source_label))
        .with_context(|| format!("无法批量读取 7Z 压缩包：{source_label}"))?;

    if let Some(error) = callback_error {
        return Err(error);
    }
    if let Some(entry_path) = remaining.keys().next() {
        bail!("未找到 7Z 条目：{entry_path}");
    }
    Ok(())
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

        let is_dir = entry.is_directory();
        entries.push(ArchiveEntryInfo {
            path: entry_path,
            is_dir,
            unpacked_size: (!is_dir).then(|| entry.size()),
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
                // 固实 7Z 的多个条目共享同一条顺序解码流：跳过的条目必须完整消费，
                // 否则后续条目会从错误偏移开始读取并触发 CRC 校验失败。
                // 找到目标条目后立即结束，目标之后的条目无需再解码。
                std::io::copy(entry_reader, &mut std::io::sink()).map_err(SevenzError::io)?;
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

/// 判断底层 IO 错误是否由 sevenz-rust 的 CRC 校验失败引起。
fn is_sevenz_checksum_failure(error: &std::io::Error) -> bool {
    error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<SevenzError>())
        .is_some_and(|inner| matches!(inner, SevenzError::ChecksumVerificationFailed))
}

/// 构造面向用户的压缩包损坏提示。
fn sevenz_checksum_error(source_label: &str) -> anyhow::Error {
    anyhow::anyhow!("压缩包数据校验失败，文件可能已损坏：{source_label}")
}

/// 将 sevenz-rust 的密码错误转换为 Argus 统一错误。
fn map_sevenz_error(error: SevenzError, source_label: &str) -> anyhow::Error {
    match error {
        SevenzError::PasswordRequired => ArchivePasswordError::required(source_label).into(),
        SevenzError::MaybeBadPassword(_) => ArchivePasswordError::invalid(source_label).into(),
        // 未加密归档的 CRC 失败是数据损坏，不能误判为密码错误并反复要求输入密码。
        // 条目解码器的 CRC 错误会被包装进 io::Error，因此需要同时检查内层错误。
        SevenzError::Io(error, _) if is_sevenz_checksum_failure(&error) => {
            sevenz_checksum_error(source_label)
        }
        SevenzError::ChecksumVerificationFailed => sevenz_checksum_error(source_label),
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

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read, Seek, SeekFrom};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sevenz_rust::{SeqReader, SevenZArchiveEntry, SevenZWriter, SourceReader};

    use super::*;

    /// 构造把多个条目放在同一解码流里的固实 7Z，用于复现跳过前序条目的场景。
    fn solid_sevenz_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = SevenZWriter::new(Cursor::new(Vec::new())).expect("创建 7Z 写入器");
        let archive_entries = entries
            .iter()
            .map(|(name, _)| {
                let mut entry = SevenZArchiveEntry::new();
                entry.name = (*name).to_string();
                entry.has_stream = true;
                entry
            })
            .collect::<Vec<_>>();
        let readers = SeqReader::new(
            entries
                .iter()
                .map(|(_, data)| SourceReader::new(Cursor::new(data.to_vec())))
                .collect::<Vec<_>>(),
        );
        writer
            .push_archive_entries(archive_entries, readers)
            .expect("写入固实 7Z 条目");
        writer.finish().expect("完成 7Z 写入").into_inner()
    }

    /// 统计底层读取字节数的游标，用于验证批量提取只扫描一次固实块。
    struct CountingReader {
        inner: Cursor<Vec<u8>>,
        read_bytes: Arc<AtomicUsize>,
    }

    impl Read for CountingReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.inner.read(buffer)?;
            self.read_bytes.fetch_add(read, Ordering::Relaxed);
            Ok(read)
        }
    }

    impl Seek for CountingReader {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    /// 批量提取必须只扫描一次固实块，而不是为每个目标条目重新解码前面的数据。
    #[test]
    fn batch_extraction_scan_solid_block_once() {
        let bytes = solid_sevenz_bytes(&[
            ("first.log", b"first entry content\n"),
            ("second.log", b"second entry content\n"),
            ("third.log", b"third entry content\n"),
        ]);
        let directory = tempfile::tempdir().expect("创建测试目录");
        let first = directory.path().join("first.log");
        let second = directory.path().join("second.log");
        let third = directory.path().join("third.log");
        let targets = vec![
            ArchiveEntryTarget {
                entry_path: "first.log".to_string(),
                target: first.clone(),
            },
            ArchiveEntryTarget {
                entry_path: "second.log".to_string(),
                target: second.clone(),
            },
            ArchiveEntryTarget {
                entry_path: "third.log".to_string(),
                target: third.clone(),
            },
        ];
        let read_bytes = Arc::new(AtomicUsize::new(0));
        let reader = CountingReader {
            inner: Cursor::new(bytes.clone()),
            read_bytes: read_bytes.clone(),
        };
        let cancellation = CancellationToken::new();

        extract_sevenz_entries_to_paths(
            reader,
            bytes.len() as u64,
            "solid.7z",
            None,
            &targets,
            1024 * 1024,
            &cancellation,
        )
        .expect("批量提取应成功");

        assert_eq!(
            std::fs::read(first).expect("读取第一个条目"),
            b"first entry content\n"
        );
        assert_eq!(
            std::fs::read(second).expect("读取第二个条目"),
            b"second entry content\n"
        );
        assert_eq!(
            std::fs::read(third).expect("读取第三个条目"),
            b"third entry content\n"
        );
        assert!(
            read_bytes.load(Ordering::Relaxed) < bytes.len() * 2,
            "批量提取应只读取一次压缩数据，实际读取 {} 字节，压缩包 {} 字节",
            read_bytes.load(Ordering::Relaxed),
            bytes.len()
        );
    }

    /// 批量提取缺少目标条目时必须明确报错，不能静默成功。
    #[test]
    fn batch_extraction_reports_missing_entry() {
        let bytes = solid_sevenz_bytes(&[("first.log", b"first")]);
        let directory = tempfile::tempdir().expect("创建测试目录");
        let targets = vec![ArchiveEntryTarget {
            entry_path: "missing.log".to_string(),
            target: directory.path().join("missing.log"),
        }];

        let error = extract_sevenz_entries_to_paths(
            Cursor::new(bytes.clone()),
            bytes.len() as u64,
            "solid.7z",
            None,
            &targets,
            1024 * 1024,
            &CancellationToken::new(),
        )
        .expect_err("缺少目标条目应失败");

        assert!(error.to_string().contains("未找到 7Z 条目"));
    }

    /// 7Z 适配器必须声明支持批量提取，工作区才会走一次扫描路径。
    #[test]
    fn sevenz_adapter_supports_batch_extraction() {
        assert!(SevenzArchiveAdapter.supports_batch_extraction());
    }

    /// 固实 7Z 中读取非首个条目时，必须先完整消费前序条目，否则会触发 CRC 校验失败。
    #[test]
    fn reading_later_solid_entry_consumes_previous_streams() {
        let bytes = solid_sevenz_bytes(&[
            ("first.log", b"first entry content\n"),
            ("second.log", b"second entry content\n"),
        ]);

        let content = read_sevenz_entry_bytes_from_reader(
            Cursor::new(bytes.clone()),
            bytes.len() as u64,
            "second.log",
            "solid.7z",
            None,
        )
        .expect("应能读取固实 7Z 的第二个条目");

        assert_eq!(content, b"second entry content\n");
    }

    /// 未加密归档的 CRC 失败必须按数据损坏报告，不能误触发密码输入流程。
    #[test]
    fn checksum_failure_is_reported_as_corruption_not_bad_password() {
        let error = map_sevenz_error(SevenzError::ChecksumVerificationFailed, "broken.7z");

        assert!(error.downcast_ref::<ArchivePasswordError>().is_none());
        assert!(error.to_string().contains("校验失败"));
    }

    /// 条目解码器会把 CRC 失败包装成 IO 错误，也必须识别为数据损坏。
    #[test]
    fn wrapped_checksum_failure_is_reported_as_corruption() {
        let io_error = std::io::Error::other(SevenzError::ChecksumVerificationFailed);
        let error = map_sevenz_error(SevenzError::Io(io_error, "".into()), "broken.7z");

        assert!(error.downcast_ref::<ArchivePasswordError>().is_none());
        assert!(error.to_string().contains("校验失败"));
    }
}
