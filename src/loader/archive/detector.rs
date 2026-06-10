//! 文件职责：识别本地文件对应的压缩包格式。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：对外提供压缩格式枚举，并将格式识别委托给压缩适配器注册表。

use std::path::Path;

use crate::loader::archive::registry::archive_registry;

/// Argus MVP 支持识别的压缩格式。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveFormat {
    /// ZIP 压缩包。
    Zip,
    /// 普通 TAR 归档。
    Tar,
    /// gzip 压缩的 TAR 归档。
    TarGz,
    /// bzip2 压缩的 TAR 归档。
    TarBz2,
    /// xz 压缩的 TAR 归档。
    TarXz,
    /// 7Z 压缩包。
    SevenZ,
    /// RAR 压缩包。
    Rar,
}

impl ArchiveFormat {
    /// 返回面向用户的格式名称。
    pub fn label(self) -> &'static str {
        archive_registry()
            .capabilities(self)
            .map(|capabilities| capabilities.label)
            .unwrap_or("未知压缩包")
    }

    /// 返回该格式是否属于当前注册表可展开范围。
    pub fn is_supported(self) -> bool {
        archive_registry().is_supported(self)
    }
}

/// 尝试识别路径对应的压缩格式。
///
/// 返回值：识别成功时返回具体格式；普通日志文件或未知文件返回 `None`。
pub fn detect_archive_format(path: &Path) -> Option<ArchiveFormat> {
    archive_registry().detect_path(path)
}

/// 仅根据文件名判断压缩格式，用于压缩包内部虚拟条目的轻量识别。
pub fn detect_archive_format_by_name(name: &str) -> Option<ArchiveFormat> {
    archive_registry().detect_name(name)
}

#[cfg(test)]
mod tests {
    use super::{ArchiveFormat, detect_archive_format_by_name};

    /// 验证压缩格式扩展名识别覆盖 MVP 格式和 RAR 降级格式。
    #[test]
    fn detects_archive_format_from_name() {
        assert_eq!(
            detect_archive_format_by_name("app.zip"),
            Some(ArchiveFormat::Zip)
        );
        assert_eq!(
            detect_archive_format_by_name("logs.tar"),
            Some(ArchiveFormat::Tar)
        );
        assert_eq!(
            detect_archive_format_by_name("logs.tar.gz"),
            Some(ArchiveFormat::TarGz)
        );
        assert_eq!(
            detect_archive_format_by_name("logs.tbz2"),
            Some(ArchiveFormat::TarBz2)
        );
        assert_eq!(
            detect_archive_format_by_name("logs.txz"),
            Some(ArchiveFormat::TarXz)
        );
        assert_eq!(
            detect_archive_format_by_name("logs.7z"),
            Some(ArchiveFormat::SevenZ)
        );
        assert_eq!(
            detect_archive_format_by_name("logs.rar"),
            Some(ArchiveFormat::Rar)
        );
        assert_eq!(detect_archive_format_by_name("app.log"), None);
    }
}
