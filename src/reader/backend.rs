//! 文件职责：定义日志读取后端的统一抽象模型。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：把本地 mmap 和压缩流式后端统一为可扩展枚举，便于后续接入新来源类型。

use anyhow::Result;

use crate::loader::SourceLocation;
use crate::reader::mmap_backend::MmapBackend;
use crate::reader::read_mode::ReadMode;
use crate::reader::stream_backend::ArchiveStreamBackend;

/// 日志读取后端枚举；新增来源类型时优先新增变体和后端模块。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadBackend {
    /// 本地普通文件 mmap 后端。
    Mmap,
    /// 压缩包内部条目流式后端。
    ArchiveStream,
}

impl ReadBackend {
    /// 根据来源位置选择读取后端。
    pub fn for_location(location: &SourceLocation) -> Self {
        match location {
            SourceLocation::LocalPath(_) => Self::Mmap,
            SourceLocation::ArchiveEntry { .. } => Self::ArchiveStream,
        }
    }

    /// 返回后端对应的读取模式，供状态栏展示。
    pub fn read_mode(self) -> ReadMode {
        match self {
            Self::Mmap => ReadMode::MmapPaged,
            Self::ArchiveStream => ReadMode::ArchiveStreaming,
        }
    }

    /// 读取来源原始字节；具体后端负责保证自己的访问约束。
    pub fn read_to_bytes(self, location: &SourceLocation) -> Result<Vec<u8>> {
        match (self, location) {
            (Self::Mmap, SourceLocation::LocalPath(path)) => MmapBackend::read_to_bytes(path),
            (Self::ArchiveStream, SourceLocation::ArchiveEntry { .. }) => {
                ArchiveStreamBackend::read_to_bytes(location)
            }
            (backend, _) => anyhow::bail!(
                "读取后端 {:?} 与来源位置不匹配：{}",
                backend,
                location.display_path()
            ),
        }
    }
}
