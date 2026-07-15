//! 文件职责：实现普通日志文件的 mmap 读取后端。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：使用只读内存映射读取本地日志文件，为后续页索引和随机访问提供基础。

use std::fs::File;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use memmap2::MmapOptions;

/// 本地普通文件 mmap 后端；当前返回完整字节供统一解码器生成文本。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MmapBackend;

impl MmapBackend {
    /// 读取本地日志文件的原始字节。
    ///
    /// 参数说明：
    /// - `path`：本地普通日志文件路径。
    ///
    /// 返回值：日志文件字节；空文件返回空 Vec。
    pub(crate) fn read_to_bytes(path: &Path) -> Result<Vec<u8>> {
        if !path.is_file() {
            bail!("日志来源不是普通文件：{}", path.display());
        }

        let file =
            File::open(path).with_context(|| format!("无法打开日志文件：{}", path.display()))?;
        let metadata = file
            .metadata()
            .with_context(|| format!("无法读取日志文件元信息：{}", path.display()))?;
        if metadata.len() == 0 {
            return Ok(Vec::new());
        }

        // SAFETY: 这里只创建只读 mmap，不写入底层文件。映射创建后复制出当前读取
        // 所需字节，后续可替换为保留 mmap 句柄并按页解码的实现。
        let mmap = unsafe { MmapOptions::new().map(&file) }
            .with_context(|| format!("无法映射日志文件：{}", path.display()))?;
        Ok(mmap.as_ref().to_vec())
    }
}
