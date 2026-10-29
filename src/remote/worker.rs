//! 远程会话的取消与资源生命周期控制；取消不依赖正在执行 I/O 的命令队列。

use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, bail};
use tokio_util::sync::CancellationToken;

/// worker 可共享的取消信号和 SSH socket；提交下载与取消在同一锁上排序。
#[derive(Clone, Default)]
pub(crate) struct WorkerCancellation {
    token: CancellationToken,
    socket: Arc<Mutex<Option<TcpStream>>>,
}

impl WorkerCancellation {
    /// 等待会话取消，用于连接和异步传输的选择分支。
    pub(super) async fn cancelled(&self) {
        self.token.cancelled().await;
    }

    /// 会话已取消时返回错误，防止继续传输或发布下载文件。
    pub(super) fn ensure_active(&self) -> Result<()> {
        if self.token.is_cancelled() {
            bail!("远程会话已取消");
        }
        Ok(())
    }

    /// 保留 socket 的独立句柄；关闭标签可直接唤醒阻塞中的 libssh2。
    pub(super) fn attach_socket(&self, socket: &TcpStream) -> Result<()> {
        let mut registered = self
            .socket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.ensure_active()?;
        *registered = Some(socket.try_clone().context("无法建立 SSH 取消句柄")?);
        Ok(())
    }

    /// 仅在未取消时完成短暂的原子提交；取消先取得锁时绝不替换用户文件。
    pub(super) fn while_active<T>(&self, commit: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = self
            .socket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.ensure_active()?;
        commit()
    }

    /// 标记取消并关闭已建立的 SSH socket；重复取消安全，正常断线不作为错误。
    fn cancel(&self) -> Result<()> {
        let mut socket = self
            .socket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.token.cancel();
        if let Some(socket) = socket.take()
            && let Err(error) = socket.shutdown(Shutdown::Both)
            && error.kind() != std::io::ErrorKind::NotConnected
        {
            return Err(error).context("无法关闭远程会话 socket");
        }
        Ok(())
    }
}

/// UI 会话持有的唯一取消守卫；标签关闭或应用实体释放都会取消后台工作。
pub(crate) struct WorkerGuard {
    session_id: usize,
    cancellation: WorkerCancellation,
}

impl WorkerGuard {
    /// 创建生命周期守卫及供后台使用的共享取消信号。
    pub(super) fn new(session_id: usize) -> (Self, WorkerCancellation) {
        let cancellation = WorkerCancellation::default();
        (
            Self {
                session_id,
                cancellation: cancellation.clone(),
            },
            cancellation,
        )
    }
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        if let Err(error) = self.cancellation.cancel() {
            eprintln!("取消远程会话失败，会话 ID={}：{error:#}", self.session_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;
    use std::time::Duration;

    #[test]
    fn dropping_guard_cancels_commit_and_unblocks_socket_read() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_server, _) = listener.accept().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let (guard, cancellation) = WorkerGuard::new(1);
        cancellation.attach_socket(&client).unwrap();
        drop(guard);
        assert!(cancellation.ensure_active().is_err());
        assert!(cancellation.while_active(|| Ok(())).is_err());
        let result = client.read(&mut [0; 1]);
        assert!(
            matches!(result, Ok(0))
                || result.is_err_and(|error| !matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ))
        );
    }
}
