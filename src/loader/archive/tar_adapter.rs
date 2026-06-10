//! 文件职责：实现 TAR 归档条目枚举适配器。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：打开普通 TAR 文件并枚举目录树所需的条目元信息。

use std::fs::File;
use std::path::Path;

use anyhow::{Context as _, Result};

use crate::loader::archive::adapter::{ArchiveAdapter, ArchiveEntryInfo};
use crate::utils::path::normalize_archive_entry_path;

/// 普通 TAR 适配器，当前只做条目枚举。
#[derive(Debug, Default)]
pub struct TarArchiveAdapter;

impl ArchiveAdapter for TarArchiveAdapter {
    /// 枚举 TAR 条目并转换为统一条目模型。
    fn list_entries(&self, path: &Path) -> Result<Vec<ArchiveEntryInfo>> {
        let file =
            File::open(path).with_context(|| format!("无法打开 TAR 归档：{}", path.display()))?;
        list_tar_entries(file, path)
    }
}

/// 从任意读取器中枚举 TAR 条目，供普通 TAR 和压缩 TAR 复用。
pub fn list_tar_entries<R>(reader: R, source_path: &Path) -> Result<Vec<ArchiveEntryInfo>>
where
    R: std::io::Read,
{
    let mut archive = tar::Archive::new(reader);
    let mut entries = Vec::new();

    for entry in archive
        .entries()
        .with_context(|| format!("无法读取 TAR 条目：{}", source_path.display()))?
    {
        let entry =
            entry.with_context(|| format!("无法解析 TAR 条目：{}", source_path.display()))?;
        let entry_path = normalize_archive_entry_path(&entry.path()?.to_string_lossy());
        if entry_path.is_empty() {
            continue;
        }

        let label = entry_path
            .rsplit('/')
            .next()
            .unwrap_or(entry_path.as_str())
            .to_string();
        entries.push(ArchiveEntryInfo {
            path: entry_path,
            label,
            is_dir: entry.header().entry_type().is_dir(),
            size: Some(entry.size()),
        });
    }

    Ok(entries)
}
