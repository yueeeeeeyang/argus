//! 工作目录租约：跨进程区分活跃会话与崩溃残留，防止启动清扫误删正在使用的日志。

use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};

/// 持有工作目录的排他租约；最后一个句柄释放或进程退出后，系统自动释放锁。
#[derive(Debug)]
pub(crate) struct WorkspaceLease {
    _file: File,
}

/// 先取得旁路租约再创建目录，保证清扫线程观察到的每个新目录都已有所有者。
pub(crate) fn create_leased_workspace(root: &Path) -> Result<Arc<WorkspaceLease>> {
    create_leased_workspace_with_observer(root, || {})
}

/// 创建观察点用于测试租约文件已经可见、但尚未取得租约锁的真实竞争窗口。
fn create_leased_workspace_with_observer(
    root: &Path,
    after_lease_created: impl FnOnce(),
) -> Result<Arc<WorkspaceLease>> {
    let parent = root.parent().context("日志工作目录缺少父目录")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("无法创建日志缓存目录：{}", parent.display()))?;
    let _coordination = lock_workspace_directory(parent)?;
    let lease_path = lease_path(root);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&lease_path)
        .with_context(|| format!("无法创建日志工作目录租约：{}", lease_path.display()))?;
    after_lease_created();
    file.lock()
        .with_context(|| format!("无法锁定日志工作目录：{}", root.display()))?;
    fs::create_dir(root).with_context(|| format!("无法创建日志工作目录：{}", root.display()))?;
    Ok(Arc::new(WorkspaceLease { _file: file }))
}

/// 仅删除能够取得租约的残留目录；活跃目录跳过，其他失败由调用方记录。
pub(crate) fn sweep_workspaces(workspaces: &Path) -> Result<()> {
    let entries = match fs::read_dir(workspaces) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("无法枚举日志缓存目录"),
    };
    for entry in entries {
        let entry = entry.context("无法读取日志缓存目录条目")?;
        let file_type = entry.file_type().context("无法读取日志缓存条目类型")?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("argus-") || file_type.is_symlink() {
            continue;
        }
        // 目录存在性和旁路租约身份必须一起检查，不能抢先删除刚创建但尚未加锁的租约。
        let coordination = lock_workspace_directory(workspaces)?;
        let (root, lease_path) = if file_type.is_dir() {
            (path.clone(), lease_path(&path))
        } else if file_type.is_file() && name.ends_with(".lease") {
            let root = path.with_extension("");
            // 有目录的租约随目录一起处理；没有目录的租约可能来自创建中或创建失败。
            if root.exists() {
                continue;
            }
            (root, path)
        } else {
            continue;
        };
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lease_path)
            .with_context(|| format!("无法检查日志工作目录租约：{}", root.display()))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => continue,
            Err(TryLockError::Error(error)) => {
                return Err(error)
                    .with_context(|| format!("无法锁定待清扫日志工作目录：{}", root.display()));
            }
        }
        // 此时已持有目标租约，较慢的递归删除不再阻塞其他工作目录的创建。
        drop(coordination);
        match fs::remove_dir_all(&root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("无法清扫日志工作目录：{}", root.display()));
            }
        }
        // 锁文件位于目录外，删除目录期间仍保持排他；目录名不会被后续会话复用。
        match fs::remove_file(&lease_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("无法清理已失效日志工作目录租约"),
        }
    }
    Ok(())
}

/// 租约放在工作目录之外，避免删除工作区内容时先释放其身份标识。
fn lease_path(root: &Path) -> PathBuf {
    root.with_extension("lease")
}

/// 协调文件永久保留且不以 argus- 开头，避免锁住的文件被清扫后换成另一 inode。
fn open_coordination_file(workspaces: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(workspaces.join(".workspace-coordination"))
        .with_context(|| format!("无法打开日志工作目录协调锁：{}", workspaces.display()))
}

/// 创建租约与清扫取得租约按同一顺序持锁，持有时间只覆盖身份发布或检查。
fn lock_workspace_directory(workspaces: &Path) -> Result<File> {
    let file = open_coordination_file(workspaces)?;
    file.lock()
        .with_context(|| format!("无法锁定日志工作目录创建与清扫：{}", workspaces.display()))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_preserves_active_workspaces_and_removes_released_ones() {
        let cache = tempfile::tempdir().unwrap();
        let active = cache.path().join("argus-active");
        let released = cache.path().join("argus-released");
        let lease = create_leased_workspace(&active).unwrap();
        let shared = lease.clone();
        drop(create_leased_workspace(&released).unwrap());
        fs::write(active.join("app.log"), "活跃日志").unwrap();
        drop(lease);
        sweep_workspaces(cache.path()).unwrap();
        assert!(active.join("app.log").exists());
        assert!(!released.exists());
        drop(shared);
        sweep_workspaces(cache.path()).unwrap();
        assert!(!active.exists());
    }

    #[test]
    fn sweep_preserves_creating_workspace_and_unrelated_entries() {
        let cache = tempfile::tempdir().unwrap();
        let root = cache.path().join("argus-creating");
        let file = File::create(lease_path(&root)).unwrap();
        file.lock().unwrap();
        let unrelated = cache.path().join("other");
        fs::create_dir(&unrelated).unwrap();
        sweep_workspaces(cache.path()).unwrap();
        assert!(lease_path(&root).exists());
        assert!(unrelated.exists());
        drop(file);
        sweep_workspaces(cache.path()).unwrap();
        assert!(!lease_path(&root).exists());
    }

    #[test]
    fn sweep_reports_io_errors_and_creation_never_reuses_a_lease() {
        let cache = tempfile::tempdir().unwrap();
        let root = cache.path().join("argus-active");
        let _lease = create_leased_workspace(&root).unwrap();
        assert!(create_leased_workspace(&root).is_err());
        let file = cache.path().join("file");
        fs::write(&file, "日志").unwrap();
        assert!(sweep_workspaces(&file).is_err());
        assert!(sweep_workspaces(&cache.path().join("missing")).is_ok());
    }

    /// 精确停在 create_new 与租约 lock 之间，清扫仍不能解绑创建者即将锁定的文件。
    #[test]
    fn sweep_cannot_unlink_lease_between_creation_and_locking() {
        let cache = tempfile::tempdir().unwrap();
        let root = cache.path().join("argus-publication-race");
        let (created_sender, created_receiver) = std::sync::mpsc::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let (sweep_sender, sweep_receiver) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let creating_root = &root;
            let creator = scope.spawn(move || {
                create_leased_workspace_with_observer(creating_root, || {
                    created_sender.send(()).unwrap();
                    release_receiver.recv().unwrap();
                })
                .unwrap()
            });
            created_receiver.recv().unwrap();
            assert!(!root.exists());
            // 证明确实停在租约锁之前，而不是只覆盖“已锁定但尚未 mkdir”的旧测试。
            let lease_probe = File::open(lease_path(&root)).unwrap();
            lease_probe.try_lock().unwrap();
            drop(lease_probe);
            let coordination = open_coordination_file(cache.path()).unwrap();
            assert!(matches!(
                coordination.try_lock(),
                Err(TryLockError::WouldBlock)
            ));
            let sweeper = scope.spawn(|| {
                sweep_sender.send(()).unwrap();
                sweep_workspaces(cache.path()).unwrap();
            });
            sweep_receiver.recv().unwrap();
            release_sender.send(()).unwrap();
            let lease = creator.join().unwrap();
            sweeper.join().unwrap();
            assert!(root.is_dir());
            assert!(lease_path(&root).is_file());
            sweep_workspaces(cache.path()).unwrap();
            assert!(root.is_dir());
            drop(lease);
        });
        sweep_workspaces(cache.path()).unwrap();
        assert!(!root.exists());
    }
}
