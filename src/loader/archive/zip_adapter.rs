//! 文件职责：实现 ZIP 压缩包条目枚举适配器。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：打开本地 ZIP 文件并枚举目录树所需的条目元信息。

use std::fs::File;
use std::path::Path;

use anyhow::{Context as _, Result};
use zip::ZipArchive;

use crate::loader::archive::adapter::{ArchiveAdapter, ArchiveEntryInfo};
use crate::utils::path::normalize_archive_entry_path;

/// ZIP 适配器，当前只做条目枚举，不读取日志正文。
#[derive(Debug, Default)]
pub struct ZipArchiveAdapter;

impl ArchiveAdapter for ZipArchiveAdapter {
    /// 枚举 ZIP 条目并转换为统一条目模型。
    fn list_entries(&self, path: &Path) -> Result<Vec<ArchiveEntryInfo>> {
        let file =
            File::open(path).with_context(|| format!("无法打开 ZIP 压缩包：{}", path.display()))?;
        let mut archive = ZipArchive::new(file)
            .with_context(|| format!("无法解析 ZIP 压缩包：{}", path.display()))?;
        let mut entries = Vec::new();

        for index in 0..archive.len() {
            let file = archive
                .by_index(index)
                .with_context(|| format!("无法读取 ZIP 第 {index} 个条目：{}", path.display()))?;
            let entry_path = normalize_archive_entry_path(file.name());
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
                is_dir: file.is_dir(),
                size: Some(file.size()),
            });
        }

        Ok(entries)
    }
}
