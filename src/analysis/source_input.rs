//! 文件职责：提供分析器共享的来源展开与单文件压缩包定位逻辑。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：统一目录稳定遍历、符号链接循环防护和压缩包探测，同时允许分析器注入候选文件规则。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use crate::config::LoaderConfig;
use crate::loader::archive::ArchivePasswordStore;
use crate::loader::{
    LogSourceLoader, SourceArchiveProbeRequest, SourceId, SourceLocation, SourceTreeNode,
};

/// 递归收集满足分析器规则的本地文件，并按完整路径稳定排序。
///
/// 参数说明：
/// - `root`：待遍历的本地目录；非目录会返回错误。
/// - `follow_symlinks`：是否进入符号链接指向的目录或接收其指向的文件。
/// - `is_candidate`：由 Jstack、Runtime 等分析器提供的候选文件判断规则。
///
/// 返回值：去除符号链接目录回环后的候选文件路径，顺序不依赖文件系统枚举顺序。
pub(super) fn collect_analysis_files(
    root: &Path,
    follow_symlinks: bool,
    is_candidate: impl Fn(&Path) -> bool + Copy,
) -> Result<Vec<PathBuf>> {
    if !root.is_dir() {
        return Err(anyhow!("{} 不是本地目录", root.display()));
    }

    let mut paths = Vec::new();
    let mut visited_dirs = BTreeSet::new();
    collect_directory_files(
        root,
        follow_symlinks,
        is_candidate,
        &mut visited_dirs,
        &mut paths,
    )?;
    paths.sort();
    Ok(paths)
}

/// 深度优先遍历单个目录；真实路径集合用于阻断符号链接形成的循环。
fn collect_directory_files(
    dir: &Path,
    follow_symlinks: bool,
    is_candidate: impl Fn(&Path) -> bool + Copy,
    visited_dirs: &mut BTreeSet<PathBuf>,
    paths: &mut Vec<PathBuf>,
) -> Result<()> {
    let canonical_dir = fs::canonicalize(dir)
        .map_err(|error| anyhow!("无法解析目录真实路径 {}：{error}", dir.display()))?;
    if !visited_dirs.insert(canonical_dir) {
        return Ok(());
    }

    let mut entries = fs::read_dir(dir)
        .map_err(|error| anyhow!("无法读取目录 {}：{error}", dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| anyhow!("无法遍历目录 {}：{error}", dir.display()))?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        let link_metadata = fs::symlink_metadata(&path)
            .map_err(|error| anyhow!("无法读取文件元数据 {}：{error}", path.display()))?;
        let is_symlink = link_metadata.file_type().is_symlink();
        if is_symlink && !follow_symlinks {
            continue;
        }

        let metadata = if is_symlink {
            fs::metadata(&path)
        } else {
            Ok(link_metadata)
        }
        .map_err(|error| anyhow!("无法读取文件元数据 {}：{error}", path.display()))?;

        if metadata.is_dir() {
            collect_directory_files(&path, follow_symlinks, is_candidate, visited_dirs, paths)?;
        } else if metadata.is_file() && is_candidate(&path) {
            paths.push(path);
        }
    }

    Ok(())
}

/// 解析分析目标的真实读取位置；普通来源直接返回，待探测压缩包只接受单文件根层。
pub(super) fn resolve_analysis_location(
    source_id: SourceId,
    location: &SourceLocation,
    archive_probe_node: Option<&SourceTreeNode>,
    archive_passwords: &ArchivePasswordStore,
    loader_config: &LoaderConfig,
) -> Result<SourceLocation> {
    let Some(node) = archive_probe_node.cloned() else {
        return Ok(location.clone());
    };

    LogSourceLoader::new(loader_config.clone())
        .with_archive_passwords(archive_passwords.clone())
        .probe_archive_nodes(vec![SourceArchiveProbeRequest { source_id, node }])
        .into_iter()
        .next()
        .and_then(|result| result.patch)
        .map(|patch| patch.location)
        .ok_or_else(|| anyhow!("压缩包根层不是单文件日志，请展开后选择具体日志条目"))
}
