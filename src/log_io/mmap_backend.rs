//! 文件职责：实现普通日志文件的只读内存映射后端。
//! 创建日期：2026-06-09
//! 修改日期：2026-10-05
//! 作者：Argus 开发团队
//! 主要功能：以只读内存映射提供日志字节视图，使解码与按行切片都不再复制整份原始字节。

use std::fs::File;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use memmap2::{Mmap, MmapOptions};

/// 只读日志映射句柄。
///
/// 设计说明：映射建立后正文按页由操作系统提供，解码与按行切片直接引用映射内存，
/// 因此不再出现“整文件复制到 `Vec<u8>` 再解码”的额外副本。
///
/// 生命周期约束：映射内容随底层文件变化而变化。Argus 的日志来源在加载阶段已物化到
/// `cache/workdirs` 下的自有副本，会话期间不会被其他进程改写，因此可以安全地长期持有；
/// 映射在打开日志时建立，失败会立刻作为打开错误上报，而不是等到滚动时才暴露。
pub(crate) struct MappedLogFile {
    /// 映射本体；空文件没有可映射区间，用 `None` 表示。
    map: Option<Mmap>,
    /// 源文件句柄：Windows 上映射的有效性依赖句柄存活，统一保留以保证跨平台行为一致。
    _file: File,
}

impl std::fmt::Debug for MappedLogFile {
    /// 只输出映射长度，避免把日志正文写进 Debug 输出或日志。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MappedLogFile")
            .field("len", &self.len())
            .finish()
    }
}

impl MappedLogFile {
    /// 建立指定普通文件的只读映射。
    ///
    /// 参数说明：
    /// - `path`：本地普通日志文件路径。
    ///
    /// 返回值：可长期持有的映射句柄；空文件返回长度为零的句柄，路径不是普通文件或映射
    /// 失败时返回带路径上下文的错误。
    pub(crate) fn open(path: &Path) -> Result<Self> {
        if !path.is_file() {
            bail!("日志来源不是普通文件：{}", path.display());
        }

        let file =
            File::open(path).with_context(|| format!("无法打开日志文件：{}", path.display()))?;
        let metadata = file
            .metadata()
            .with_context(|| format!("无法读取日志文件元信息：{}", path.display()))?;
        // mmap 不接受长度为 0 的映射，空日志用空切片语义表达。
        if metadata.len() == 0 {
            return Ok(Self {
                map: None,
                _file: file,
            });
        }

        // SAFETY: 只建立只读映射，不写入底层文件。来源文件属于 Argus 自有的工作目录副本，
        // 会话期间不会被外部截断；长度变化只会导致读到更新后的内容，不会越界访问。
        let map = unsafe { MmapOptions::new().map(&file) }
            .with_context(|| format!("无法映射日志文件：{}", path.display()))?;
        Ok(Self {
            map: Some(map),
            _file: file,
        })
    }

    /// 返回映射后的完整字节视图；空文件返回空切片。
    pub(crate) fn as_slice(&self) -> &[u8] {
        self.map.as_deref().unwrap_or(&[])
    }

    /// 返回映射字节长度。
    pub(crate) fn len(&self) -> usize {
        self.as_slice().len()
    }

    /// 按字节范围返回映射内的切片。
    ///
    /// 参数说明：
    /// - `offset`：起始字节偏移。
    /// - `len`：需要读取的字节数。
    ///
    /// 返回值：命中范围时返回借用切片；越界或范围溢出返回错误，避免切片 panic。
    pub(crate) fn byte_span(&self, offset: u64, len: u64) -> Result<&[u8]> {
        let bytes = self.as_slice();
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let length = usize::try_from(len).unwrap_or(usize::MAX);
        let end = start
            .checked_add(length)
            .ok_or_else(|| anyhow::anyhow!("映射读取范围溢出：offset={offset}, len={len}"))?;
        bytes
            .get(start..end)
            .ok_or_else(|| anyhow::anyhow!("映射读取越界：offset={offset}, len={len}"))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::config::paths::{isolated_test_dir, isolated_test_file_path};

    /// 验证映射内容与文件一致，且按范围切片命中相同字节。
    #[test]
    fn mapped_file_exposes_file_bytes_without_copy() {
        let path = isolated_test_file_path("mmap-span", "sample.log");
        let mut file = File::create(&path).expect("应能创建测试文件");
        file.write_all(b"first\nsecond\n")
            .expect("应能写入测试内容");
        drop(file);

        let mapped = MappedLogFile::open(&path).expect("应能映射普通文件");
        assert_eq!(mapped.len(), 13);
        assert_eq!(mapped.as_slice(), b"first\nsecond\n");
        assert_eq!(mapped.byte_span(6, 6).expect("范围内切片应成功"), b"second");
        assert!(mapped.byte_span(10, 100).is_err(), "越界切片必须报错");

        let _ = std::fs::remove_file(path);
    }

    /// 验证空文件返回空视图而不是映射失败。
    #[test]
    fn mapped_file_handles_empty_file() {
        let path = isolated_test_file_path("mmap-empty", "empty.log");
        File::create(&path).expect("应能创建空测试文件");

        let mapped = MappedLogFile::open(&path).expect("空文件应可打开");
        assert_eq!(mapped.len(), 0);
        assert_eq!(mapped.as_slice(), b"");

        let _ = std::fs::remove_file(path);
    }

    /// 验证目录等非普通文件会被明确拒绝。
    #[test]
    fn mapped_file_rejects_non_regular_file() {
        let directory = isolated_test_dir("mmap-dir");
        std::fs::create_dir_all(&directory).expect("应能创建测试目录");
        assert!(MappedLogFile::open(&directory).is_err());
    }
}
