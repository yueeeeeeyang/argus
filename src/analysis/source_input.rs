//! 文件职责：提供分析器共享的来源目录遍历逻辑。
//! 创建日期：2026-07-15
//! 修改日期：2026-09-07
//! 作者：Argus 开发团队
//! 主要功能：统一目录稳定遍历和符号链接循环防护，同时允许分析器注入候选文件规则。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, anyhow};

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
    collect_analysis_files_cancellable(
        root,
        follow_symlinks,
        is_candidate,
        &AtomicBool::new(false),
    )?
    .ok_or_else(|| anyhow!("分析目录遍历被取消"))
}

/// 可取消的目录收集；取消返回 `None`，不发布部分目录结果。
pub(super) fn collect_analysis_files_cancellable(
    root: &Path,
    follow_symlinks: bool,
    is_candidate: impl Fn(&Path) -> bool + Copy,
    cancel_token: &AtomicBool,
) -> Result<Option<Vec<PathBuf>>> {
    if cancel_token.load(Ordering::Relaxed) {
        return Ok(None);
    }
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
        cancel_token,
    )?;
    if cancel_token.load(Ordering::Relaxed) {
        return Ok(None);
    }
    paths.sort();
    Ok((!cancel_token.load(Ordering::Relaxed)).then_some(paths))
}

/// 深度优先遍历单个目录；真实路径集合用于阻断符号链接形成的循环。
fn collect_directory_files(
    dir: &Path,
    follow_symlinks: bool,
    is_candidate: impl Fn(&Path) -> bool + Copy,
    visited_dirs: &mut BTreeSet<PathBuf>,
    paths: &mut Vec<PathBuf>,
    cancel_token: &AtomicBool,
) -> Result<()> {
    if cancel_token.load(Ordering::Relaxed) {
        return Ok(());
    }
    let canonical_dir = fs::canonicalize(dir)
        .map_err(|error| anyhow!("无法解析目录真实路径 {}：{error}", dir.display()))?;
    if !visited_dirs.insert(canonical_dir) {
        return Ok(());
    }

    let mut entries = Vec::new();
    for entry in
        fs::read_dir(dir).map_err(|error| anyhow!("无法读取目录 {}：{error}", dir.display()))?
    {
        if cancel_token.load(Ordering::Relaxed) {
            return Ok(());
        }
        let entry = entry.map_err(|error| anyhow!("无法遍历目录 {}：{error}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| anyhow!("无法读取文件元数据 {}：{error}", path.display()))?;
        entries.push((path, file_type));
    }
    // 路径只构造一次；保持目录内排序，确保符号链接别名始终选择同一个访问入口。
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    for (path, file_type) in entries {
        if cancel_token.load(Ordering::Relaxed) {
            return Ok(());
        }
        let is_symlink = file_type.is_symlink();
        if is_symlink && !follow_symlinks {
            continue;
        }

        let file_type = if is_symlink {
            fs::metadata(&path)
                .map_err(|error| anyhow!("无法读取文件元数据 {}：{error}", path.display()))?
                .file_type()
        } else {
            file_type
        };

        if file_type.is_dir() {
            collect_directory_files(
                &path,
                follow_symlinks,
                is_candidate,
                visited_dirs,
                paths,
                cancel_token,
            )?;
        } else if file_type.is_file() && is_candidate(&path) {
            paths.push(path);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::paths::isolated_test_dir;

    /// 候选判断中触发取消模拟遍历期间的用户取消，不能返回已收集的部分文件。
    #[test]
    fn cancellation_during_directory_collection_discards_partial_paths() {
        let dir = isolated_test_dir("analysis-source-cancel");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.log"), "").unwrap();
        fs::write(dir.join("b.log"), "").unwrap();
        let cancel = AtomicBool::new(false);
        let paths = collect_analysis_files_cancellable(
            &dir,
            false,
            |_| {
                cancel.store(true, Ordering::Relaxed);
                true
            },
            &cancel,
        )
        .unwrap();
        assert!(paths.is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    /// 空目录是成功空结果，非目录仍返回真实错误，取消优先停止尚未开始的遍历。
    #[test]
    fn directory_collection_distinguishes_empty_failure_and_cancellation() {
        let dir = isolated_test_dir("analysis-source-empty");
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            collect_analysis_files_cancellable(&dir, false, |_| true, &AtomicBool::new(false))
                .unwrap(),
            Some(Vec::new())
        );
        let absent = dir.join("missing");
        assert!(collect_analysis_files(&absent, false, |_| true).is_err());
        assert!(
            collect_analysis_files_cancellable(&absent, false, |_| true, &AtomicBool::new(true))
                .unwrap()
                .is_none()
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
