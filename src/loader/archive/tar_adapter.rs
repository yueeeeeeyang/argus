//! 文件职责：实现 TAR 归档条目枚举适配器。
//! 创建日期：2026-06-09
//! 修改日期：2026-10-27
//! 作者：Argus 开发团队
//! 主要功能：打开普通 TAR、枚举条目元信息，并按条目读取或流式输出日志内容。

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context as _, Result, bail};

use crate::loader::archive::adapter::{
    ArchiveAdapter, ArchiveCapabilities, ArchiveEntryConsumer, ArchiveEntryInfo, ArchiveEntryTarget,
};
use crate::loader::archive::detector::ArchiveFormat;
use crate::utils::path::normalize_archive_entry_path;
use tokio_util::sync::CancellationToken;

/// 普通 TAR 适配器，当前只做条目枚举。
#[derive(Debug, Default)]
pub(crate) struct TarArchiveAdapter;

impl ArchiveAdapter for TarArchiveAdapter {
    fn supports_batch_extraction(&self) -> bool {
        true
    }

    fn extract_entries_to_paths(
        &self,
        path: &Path,
        targets: &[ArchiveEntryTarget],
        max_log_bytes: u64,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        let file =
            File::open(path).with_context(|| format!("无法打开 TAR 归档：{}", path.display()))?;
        extract_tar_entries_to_paths(file, targets, max_log_bytes, cancellation)
    }
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
        }
    }

    /// TAR 的 ustar 魔数位于头部偏移 257 处。
    fn matches_header(&self, sample: &[u8]) -> bool {
        sample.get(257..262) == Some(b"ustar")
    }

    /// 枚举 TAR 条目并转换为统一条目模型。
    fn list_entries(&self, path: &Path, _password: Option<&str>) -> Result<Vec<ArchiveEntryInfo>> {
        let file =
            File::open(path).with_context(|| format!("无法打开 TAR 归档：{}", path.display()))?;
        list_tar_entries(file, &path.display().to_string())
    }

    /// 从本地 TAR 读取指定条目字节。
    fn read_entry_bytes(
        &self,
        path: &Path,
        entry_path: &str,
        _password: Option<&str>,
    ) -> Result<Vec<u8>> {
        let file =
            File::open(path).with_context(|| format!("无法打开 TAR 归档：{}", path.display()))?;
        read_tar_entry_bytes(file, entry_path, &path.display().to_string())
    }

    /// 从本地 TAR 流式读取指定条目内容。
    fn stream_entry(
        &self,
        path: &Path,
        entry_path: &str,
        _password: Option<&str>,
        consumer: &mut ArchiveEntryConsumer<'_>,
    ) -> Result<()> {
        let file =
            File::open(path).with_context(|| format!("无法打开 TAR 归档：{}", path.display()))?;
        stream_tar_entry(file, entry_path, &path.display().to_string(), consumer)
    }
}

/// 在一次顺序扫描中完成全部落盘，跳过的条目也通过可取消读取器消费。
pub(crate) fn extract_tar_entries_to_paths<R: Read>(
    reader: R,
    targets: &[ArchiveEntryTarget],
    max_log_bytes: u64,
    cancellation: &CancellationToken,
) -> Result<()> {
    let mut remaining = std::collections::HashMap::new();
    for target in targets {
        if remaining
            .insert(target.entry_path.as_str(), target.target.as_path())
            .is_some()
        {
            bail!("批量提取条目重复：{}", target.entry_path);
        }
    }
    let reader = CancellableArchiveReader {
        reader,
        cancellation,
    };
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries().context("无法读取 TAR 条目")? {
        let mut entry = entry.context("无法解析 TAR 条目")?;
        let path = normalize_archive_entry_path(&entry.path()?.to_string_lossy());
        let Some(target) = remaining.remove(path.as_str()) else {
            continue;
        };
        if entry.header().entry_type().is_dir() {
            bail!("TAR 条目是目录，无法读取内容：{path}");
        }
        if entry.size() > max_log_bytes {
            bail!("TAR 条目超过最大日志大小：{path}");
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut output = File::create_new(target)
            .with_context(|| format!("无法创建物化文件：{}", target.display()))?;
        std::io::copy(&mut entry, &mut output)
            .with_context(|| format!("无法提取 TAR 条目：{path}"))?;
    }
    // TAR 结束标记可能早于压缩流尾部；消费余量才能验证后续 gzip member 和校验和。
    std::io::copy(&mut archive.into_inner(), &mut std::io::sink())?;
    if let Some(path) = remaining.keys().next() {
        bail!("未找到 TAR 条目：{path}");
    }
    Ok(())
}

/// 取消覆盖读取与跳过两条路径，不能只在已选条目的写入回调检查。
struct CancellableArchiveReader<'a, R> {
    reader: R,
    cancellation: &'a CancellationToken,
}

impl<R: Read> Read for CancellableArchiveReader<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if self.cancellation.is_cancelled() {
            return Err(std::io::Error::other("已取消归档提取"));
        }
        self.reader.read(bytes)
    }
}

/// 从任意读取器中枚举 TAR 条目，供普通 TAR 和压缩 TAR 复用。
pub(crate) fn list_tar_entries<R>(reader: R, source_label: &str) -> Result<Vec<ArchiveEntryInfo>>
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

        let is_dir = entry.header().entry_type().is_dir();
        entries.push(ArchiveEntryInfo {
            path: entry_path,
            is_dir,
            unpacked_size: (!is_dir).then(|| entry.size()),
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
pub(crate) fn read_tar_entry_bytes<R>(
    reader: R,
    entry_path: &str,
    source_label: &str,
) -> Result<Vec<u8>>
where
    R: std::io::Read,
{
    let mut bytes = Vec::new();
    stream_tar_entry(reader, entry_path, source_label, &mut |chunk| {
        bytes.extend_from_slice(chunk);
        Ok(())
    })?;
    Ok(bytes)
}

/// 从任意读取器中流式读取 TAR 指定条目的字节。
///
/// 参数说明：
/// - `reader`：TAR 数据来源。
/// - `entry_path`：目标条目路径，统一使用 `/` 分隔。
/// - `source_label`：错误提示中的来源名称。
/// - `consumer`：接收解压后字节分片的回调。
pub(crate) fn stream_tar_entry<R>(
    reader: R,
    entry_path: &str,
    source_label: &str,
    consumer: &mut ArchiveEntryConsumer<'_>,
) -> Result<()>
where
    R: std::io::Read,
{
    let normalized_entry_path = normalize_archive_entry_path(entry_path);
    let mut archive = tar::Archive::new(reader);
    let mut buffer = [0_u8; 64 * 1024];

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

        loop {
            let read_count = entry
                .read(&mut buffer)
                .with_context(|| format!("无法读取 TAR 条目内容：{normalized_entry_path}"))?;
            if read_count == 0 {
                break;
            }
            consumer(&buffer[..read_count])?;
        }
        return Ok(());
    }

    bail!("未找到 TAR 条目：{normalized_entry_path}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    fn tar_bytes(count: usize) -> Vec<u8> {
        let mut archive = tar::Builder::new(Vec::new());
        for index in 0..count {
            let mut header = tar::Header::new_gnu();
            header.set_size(1024);
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(
                    &mut header,
                    format!("{index}.log"),
                    std::io::repeat(b'x').take(1024),
                )
                .unwrap();
        }
        archive.into_inner().unwrap()
    }

    struct CountReader<R> {
        inner: R,
        bytes: Rc<Cell<usize>>,
    }
    impl<R: Read> Read for CountReader<R> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.inner.read(buffer)?;
            self.bytes.set(self.bytes.get() + read);
            Ok(read)
        }
    }

    /// 条目数增加仍只读一次输入；该断言直接保护线性复杂度，不依赖机器性能。
    #[test]
    fn batch_tar_extraction_reads_archive_once() {
        let directory = tempfile::tempdir().unwrap();
        let archive = tar_bytes(100);
        let bytes = Rc::new(Cell::new(0));
        let reader = CountReader {
            inner: archive.as_slice(),
            bytes: bytes.clone(),
        };
        let targets = (0..100)
            .map(|index| ArchiveEntryTarget {
                entry_path: format!("{index}.log"),
                target: directory.path().join(format!("{index}.log")),
            })
            .collect::<Vec<_>>();
        extract_tar_entries_to_paths(reader, &targets, 1024, &CancellationToken::new()).unwrap();
        assert_eq!(bytes.get(), archive.len());
        assert_eq!(
            std::fs::read(&targets[99].target).unwrap(),
            vec![b'x'; 1024]
        );
    }

    /// 取消、缺失、大小超限与目标已存在都必须报告失败，已有文件不可截断。
    #[test]
    fn batch_tar_extraction_reports_failures_without_overwriting() {
        let directory = tempfile::tempdir().unwrap();
        let archive = tar_bytes(1);
        let target = ArchiveEntryTarget {
            entry_path: "0.log".into(),
            target: directory.path().join("0.log"),
        };
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert!(
            extract_tar_entries_to_paths(
                archive.as_slice(),
                std::slice::from_ref(&target),
                1024,
                &cancellation
            )
            .is_err()
        );
        assert!(!target.target.exists());
        assert!(
            extract_tar_entries_to_paths(
                archive.as_slice(),
                std::slice::from_ref(&target),
                1,
                &CancellationToken::new()
            )
            .is_err()
        );
        assert!(!target.target.exists());
        std::fs::write(&target.target, b"existing").unwrap();
        assert!(
            extract_tar_entries_to_paths(
                archive.as_slice(),
                std::slice::from_ref(&target),
                1024,
                &CancellationToken::new()
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&target.target).unwrap(), b"existing");
        let missing = ArchiveEntryTarget {
            entry_path: "missing.log".into(),
            target: directory.path().join("missing.log"),
        };
        assert!(
            extract_tar_entries_to_paths(
                archive.as_slice(),
                &[missing],
                1024,
                &CancellationToken::new()
            )
            .is_err()
        );
        assert!(
            extract_tar_entries_to_paths(
                &archive[..600],
                &[ArchiveEntryTarget {
                    entry_path: target.entry_path,
                    target: directory.path().join("truncated.log")
                }],
                1024,
                &CancellationToken::new()
            )
            .is_err()
        );
    }

    /// 取消发生在扫描途中时，跳过未选条目也必须停止，不能继续解压整个归档。
    #[test]
    fn batch_tar_extraction_observes_cancellation_while_skipping_entries() {
        struct CancelAfterRead<'a> {
            bytes: &'a [u8],
            cancellation: CancellationToken,
        }
        impl Read for CancelAfterRead<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                let read = self.bytes.read(buffer)?;
                self.cancellation.cancel();
                Ok(read)
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let archive = tar_bytes(2);
        let cancellation = CancellationToken::new();
        let target = ArchiveEntryTarget {
            entry_path: "1.log".into(),
            target: directory.path().join("1.log"),
        };
        let reader = CancelAfterRead {
            bytes: &archive,
            cancellation: cancellation.clone(),
        };
        assert!(
            extract_tar_entries_to_paths(
                reader,
                std::slice::from_ref(&target),
                1024,
                &cancellation
            )
            .is_err()
        );
        assert!(!target.target.exists());
    }
}
