//! 文件职责：定义日志读取后端的运行模式。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：区分本地 mmap 分页读取和压缩包条目流式读取，供 UI 状态栏展示。

/// 日志正文读取模式，UI 通过该模式展示当前读取策略。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReadMode {
    /// 本地普通日志文件，使用只读 mmap 读取并解码为文本。
    MmapPaged,
    /// 压缩包内部日志条目，直接从压缩适配器流式读取，不创建临时文件。
    ArchiveStreaming,
    /// 压缩包内部超大日志已物化到受控缓存文件，并复用本地分页读取。
    ArchiveSpooledPaged,
}

impl ReadMode {
    /// 返回面向状态栏的中文文案。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::MmapPaged => "mmap 分页",
            Self::ArchiveStreaming => "压缩流式",
            Self::ArchiveSpooledPaged => "压缩分页",
        }
    }
}
