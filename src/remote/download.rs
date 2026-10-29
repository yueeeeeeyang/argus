//! 所有远程协议共享的下载提交边界；失败或取消只丢弃临时文件，不破坏已有目标。

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use tempfile::NamedTempFile;

use super::worker::WorkerCancellation;

/// 在目标同目录写入的下载事务；仅显式提交才使最终路径可见。
pub(super) struct DownloadFile {
    temporary: NamedTempFile,
    destination: PathBuf,
}

impl DownloadFile {
    /// 创建同目录临时文件；目标不会在传输开始时被截断。
    pub(super) fn new(destination: &Path) -> Result<Self> {
        let parent = destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let temporary = tempfile::Builder::new()
            .prefix(".argus-download-")
            .tempfile_in(parent)
            .with_context(|| format!("无法创建下载临时文件：{}", destination.display()))?;
        Ok(Self {
            temporary,
            destination: destination.to_path_buf(),
        })
    }

    /// 返回当前临时文件，供同步或异步协议按块写入。
    pub(super) fn file(&mut self) -> &mut File {
        self.temporary.as_file_mut()
    }

    /// 复制同步字节流；每个分块检查取消，任何读取或写入错误都会保留原目标。
    pub(super) fn copy_from(
        &mut self,
        reader: &mut impl Read,
        cancellation: &WorkerCancellation,
    ) -> Result<()> {
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            cancellation.ensure_active()?;
            let count = match reader.read(&mut buffer) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                result => result.context("读取下载内容失败")?,
            };
            if count == 0 {
                return Ok(());
            }
            self.file()
                .write_all(&buffer[..count])
                .context("写入下载临时文件失败")?;
        }
    }

    /// 同步磁盘内容后原子替换目标；取消与提交有明确先后关系。
    pub(super) fn commit(self, cancellation: &WorkerCancellation) -> Result<()> {
        cancellation.ensure_active()?;
        self.temporary
            .as_file()
            .sync_all()
            .context("同步下载临时文件失败")?;
        cancellation.while_active(|| {
            self.temporary
                .persist(&self.destination)
                .with_context(|| format!("提交下载文件失败：{}", self.destination.display()))?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::worker::WorkerGuard;
    use std::io::{self, Cursor};

    struct InterruptedRead(bool);
    impl Read for InterruptedRead {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.0 {
                return Err(io::ErrorKind::ConnectionReset.into());
            }
            self.0 = true;
            buffer[..3].copy_from_slice(b"new");
            Ok(3)
        }
    }

    #[test]
    fn successful_download_atomically_replaces_existing_file_and_handles_empty_content() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("report.log");
        std::fs::write(&path, b"old").unwrap();
        let cancellation = WorkerCancellation::default();
        for content in [b"complete".as_slice(), b"".as_slice()] {
            let mut download = DownloadFile::new(&path).unwrap();
            download
                .copy_from(&mut Cursor::new(content), &cancellation)
                .unwrap();
            download.commit(&cancellation).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), content);
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn failed_or_cancelled_download_preserves_original_and_removes_temporary_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("report.log");
        std::fs::write(&path, b"original").unwrap();
        let (guard, cancellation) = WorkerGuard::new(1);
        {
            let mut download = DownloadFile::new(&path).unwrap();
            assert!(
                download
                    .copy_from(&mut InterruptedRead(false), &cancellation)
                    .is_err()
            );
        }
        let mut download = DownloadFile::new(&path).unwrap();
        download
            .file()
            .write_all(b"complete but cancelled")
            .unwrap();
        drop(guard);
        assert!(download.commit(&cancellation).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn commit_failure_preserves_existing_directory() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("existing");
        std::fs::create_dir(&path).unwrap();
        let download = DownloadFile::new(&path).unwrap();
        assert!(download.commit(&WorkerCancellation::default()).is_err());
        assert!(path.is_dir());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn download_replaces_destination_symlink_without_writing_outside_directory() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside.log");
        std::fs::write(&outside, b"original outside").unwrap();
        let directory = root.path().join("downloads");
        std::fs::create_dir(&directory).unwrap();
        let destination = directory.join("report.log");
        std::os::unix::fs::symlink(&outside, &destination).unwrap();
        let cancellation = WorkerCancellation::default();
        let mut download = DownloadFile::new(&destination).unwrap();
        download
            .copy_from(&mut Cursor::new(b"new"), &cancellation)
            .unwrap();
        download.commit(&cancellation).unwrap();
        assert_eq!(std::fs::read(&outside).unwrap(), b"original outside");
        assert_eq!(std::fs::read(&destination).unwrap(), b"new");
        assert!(
            !std::fs::symlink_metadata(destination)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}
