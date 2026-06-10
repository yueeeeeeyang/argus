//! 文件职责：实现 7Z 压缩包条目枚举适配器。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：使用 sevenz-rust 枚举 7Z 条目元信息，不读取日志正文。

use std::fs::File;
use std::path::Path;

use anyhow::{Context as _, Result};
use sevenz_rust::{Password, SevenZReader};

use crate::loader::archive::adapter::{ArchiveAdapter, ArchiveEntryInfo};
use crate::utils::path::normalize_archive_entry_path;

/// 7Z 适配器，当前只做条目枚举；加密压缩包会由底层库返回错误。
#[derive(Debug, Default)]
pub struct SevenzArchiveAdapter;

impl ArchiveAdapter for SevenzArchiveAdapter {
    /// 枚举 7Z 条目并转换为统一条目模型。
    fn list_entries(&self, path: &Path) -> Result<Vec<ArchiveEntryInfo>> {
        let file =
            File::open(path).with_context(|| format!("无法打开 7Z 压缩包：{}", path.display()))?;
        let reader_len = file
            .metadata()
            .with_context(|| format!("无法读取 7Z 文件大小：{}", path.display()))?
            .len();
        let reader = SevenZReader::new(file, reader_len, Password::empty())
            .with_context(|| format!("无法解析 7Z 压缩包：{}", path.display()))?;
        let mut entries = Vec::new();

        for entry in reader.archive().files.iter() {
            let entry_path = normalize_archive_entry_path(entry.name());
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
                is_dir: entry.is_directory(),
                size: Some(entry.size()),
            });
        }

        Ok(entries)
    }
}
