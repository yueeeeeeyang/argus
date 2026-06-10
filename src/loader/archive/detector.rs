//! 文件职责：识别本地文件对应的压缩包格式。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：基于文件头优先、扩展名兜底识别 ZIP、TAR、压缩 TAR 和 7Z 等格式。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

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
    /// RAR 当前只识别并提示不支持。
    Rar,
}

impl ArchiveFormat {
    /// 返回面向用户的格式名称。
    pub fn label(self) -> &'static str {
        match self {
            Self::Zip => "ZIP",
            Self::Tar => "TAR",
            Self::TarGz => "tar.gz",
            Self::TarBz2 => "tar.bz2",
            Self::TarXz => "tar.xz",
            Self::SevenZ => "7Z",
            Self::Rar => "RAR",
        }
    }

    /// 返回该格式是否属于当前 MVP 可展开范围。
    pub fn is_supported(self) -> bool {
        !matches!(self, Self::Rar)
    }
}

/// 尝试识别路径对应的压缩格式。
///
/// 返回值：识别成功时返回具体格式；普通日志文件或未知文件返回 `None`。
pub fn detect_archive_format(path: &Path) -> Option<ArchiveFormat> {
    detect_by_header(path).or_else(|| detect_by_extension(path))
}

/// 仅根据文件名判断压缩格式，用于压缩包内部虚拟条目的轻量识别。
pub fn detect_archive_format_by_name(name: &str) -> Option<ArchiveFormat> {
    detect_name_by_extension(&name.to_ascii_lowercase())
}

/// 通过文件头识别压缩包格式，文件头判断比扩展名更可信。
fn detect_by_header(path: &Path) -> Option<ArchiveFormat> {
    let mut file = File::open(path).ok()?;
    let mut header = [0_u8; 265];
    let bytes_read = file.read(&mut header).ok()?;
    let sample = &header[..bytes_read];

    if sample.starts_with(b"PK\x03\x04")
        || sample.starts_with(b"PK\x05\x06")
        || sample.starts_with(b"PK\x07\x08")
    {
        return Some(ArchiveFormat::Zip);
    }
    if sample.starts_with(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]) {
        return Some(ArchiveFormat::SevenZ);
    }
    if sample.starts_with(&[0x1F, 0x8B]) {
        return Some(ArchiveFormat::TarGz);
    }
    if sample.starts_with(b"BZh") {
        return Some(ArchiveFormat::TarBz2);
    }
    if sample.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]) {
        return Some(ArchiveFormat::TarXz);
    }
    if sample.starts_with(b"Rar!\x1A\x07") {
        return Some(ArchiveFormat::Rar);
    }

    if file.seek(SeekFrom::Start(257)).is_ok() {
        let mut tar_magic = [0_u8; 5];
        if file.read_exact(&mut tar_magic).is_ok() && &tar_magic == b"ustar" {
            return Some(ArchiveFormat::Tar);
        }
    }

    None
}

/// 通过扩展名识别压缩包格式，作为文件头不可读时的兜底策略。
fn detect_by_extension(path: &Path) -> Option<ArchiveFormat> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    detect_name_by_extension(&name)
}

/// 根据已标准化的小写文件名识别压缩格式。
fn detect_name_by_extension(name: &str) -> Option<ArchiveFormat> {
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        Some(ArchiveFormat::TarGz)
    } else if name.ends_with(".tar.bz2") || name.ends_with(".tbz2") || name.ends_with(".tbz") {
        Some(ArchiveFormat::TarBz2)
    } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
        Some(ArchiveFormat::TarXz)
    } else if name.ends_with(".zip") {
        Some(ArchiveFormat::Zip)
    } else if name.ends_with(".tar") {
        Some(ArchiveFormat::Tar)
    } else if name.ends_with(".7z") {
        Some(ArchiveFormat::SevenZ)
    } else if name.ends_with(".rar") {
        Some(ArchiveFormat::Rar)
    } else {
        None
    }
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
