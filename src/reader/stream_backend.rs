//! 文件职责：实现压缩包内部日志条目的顺序流读取后端。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：通过压缩适配器 `stream_entry` 直接读取条目内容，避免正文落盘生成临时文件。

use anyhow::{Result, bail};

use crate::loader::SourceLocation;
use crate::loader::archive::{
    ArchiveEntryConsumer, ArchivePasswordStore, stream_archive_entry_with_passwords,
};

/// 压缩包条目顺序流后端；当前将流式输出汇聚为解码缓冲。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ArchiveStreamBackend;

impl ArchiveStreamBackend {
    /// 流式读取压缩包内部日志条目的原始字节。
    ///
    /// 参数说明：
    /// - `location`：必须是 `SourceLocation::ArchiveEntry`。
    ///
    /// 返回值：目标条目解压后字节；读取过程中不创建临时文件。
    pub(crate) fn read_to_bytes(
        location: &SourceLocation,
        archive_passwords: &ArchivePasswordStore,
    ) -> Result<Vec<u8>> {
        let SourceLocation::ArchiveEntry {
            archive_path,
            root_format,
            container_entries,
            entry_path,
            ..
        } = location
        else {
            bail!("压缩流后端收到非压缩来源：{}", location.display_path());
        };

        let mut bytes = Vec::new();
        stream_archive_entry_with_passwords(
            archive_path,
            *root_format,
            container_entries,
            entry_path,
            archive_passwords,
            &mut |chunk| {
                bytes.extend_from_slice(chunk);
                Ok(())
            },
        )?;

        Ok(bytes)
    }

    /// 将压缩包内部日志条目按 chunk 输出给调用方。
    ///
    /// 参数说明：
    /// - `location`：必须是 `SourceLocation::ArchiveEntry`。
    /// - `consumer`：接收解压后字节分片的回调。
    ///
    /// 返回值：读取成功返回 `Ok(())`；调用方可在回调内决定继续保存在内存或物化到分页文件。
    pub(crate) fn stream_to_consumer(
        location: &SourceLocation,
        archive_passwords: &ArchivePasswordStore,
        consumer: &mut ArchiveEntryConsumer<'_>,
    ) -> Result<()> {
        let SourceLocation::ArchiveEntry {
            archive_path,
            root_format,
            container_entries,
            entry_path,
            ..
        } = location
        else {
            bail!("压缩流后端收到非压缩来源：{}", location.display_path());
        };

        stream_archive_entry_with_passwords(
            archive_path,
            *root_format,
            container_entries,
            entry_path,
            archive_passwords,
            consumer,
        )
    }
}
