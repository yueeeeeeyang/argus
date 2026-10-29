//! SVN 证书确认的会话边界：握手失败后等待独立审批通道，关闭标签或超时立即撤销请求。

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use async_channel::Sender;

use super::remote_file::RemoteFileEvent;
use super::svn_tls::SvnCertificatePrompt;
use super::worker::WorkerCancellation;

/// 用户不处理弹窗时释放后台等待；取消信号以短间隔检查，不依赖普通命令队列。
const SVN_CERTIFICATE_APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);
const SVN_CERTIFICATE_CANCEL_INTERVAL: Duration = Duration::from_millis(100);

/// 三种显式证书确认结果；永久信任由 UI 成功持久化后才发送给 worker。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SvnCertificateDecision {
    Reject,
    TrustOnce,
    TrustPermanently,
}

/// 单次不可复用的确认请求，避免旧弹窗审批新的证书或已关闭的会话。
#[derive(Clone)]
pub(crate) struct SvnCertificateApproval {
    pub request_id: String,
    pub prompt: SvnCertificatePrompt,
    response_sender: mpsc::SyncSender<SvnCertificateDecision>,
    pending: Arc<AtomicBool>,
    cancellation: WorkerCancellation,
}

impl fmt::Debug for SvnCertificateApproval {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SvnCertificateApproval")
            .field("request_id", &self.request_id)
            .field("prompt", &self.prompt)
            .field("is_pending", &self.is_pending())
            .finish()
    }
}

impl SvnCertificateApproval {
    /// 仅在当前请求尚未收到响应且其会话未取消时允许显示确认动作。
    pub(crate) fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire) && self.cancellation.ensure_active().is_ok()
    }

    /// 非阻塞地响应一次；过期、重复或已断开的通道都返回明确错误。
    pub(crate) fn respond(&self, decision: SvnCertificateDecision) -> Result<()> {
        self.cancellation.ensure_active()?;
        if !self.pending.swap(false, Ordering::AcqRel) {
            bail!("SVN 证书确认请求已结束，请重新连接");
        }
        self.response_sender
            .try_send(decision)
            .context("SVN 证书确认请求已失效，请重新连接")
    }

    fn new(
        prompt: SvnCertificatePrompt,
        cancellation: WorkerCancellation,
    ) -> (Self, mpsc::Receiver<SvnCertificateDecision>) {
        let (response_sender, response_receiver) = mpsc::sync_channel(1);
        (
            Self {
                request_id: uuid::Uuid::new_v4().to_string(),
                prompt,
                response_sender,
                pending: Arc::new(AtomicBool::new(true)),
                cancellation,
            },
            response_receiver,
        )
    }

    /// 构造隔离的审批通道，供应用层测试验证真实的批准和拒绝行为。
    #[cfg(test)]
    pub(crate) fn test_request(
        prompt: SvnCertificatePrompt,
    ) -> (Self, mpsc::Receiver<SvnCertificateDecision>) {
        Self::new(prompt, WorkerCancellation::default())
    }
}

/// 等待退出时使所有 UI 请求副本失效，包括发送失败、超时和会话关闭路径。
struct PendingApprovalGuard(Arc<AtomicBool>);

impl Drop for PendingApprovalGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// 每个 SVN worker 独立持有的证书确认上下文，不共享连接或证书信任。
pub(super) struct SvnCertificateContext {
    pub session_id: usize,
    pub event_sender: Sender<RemoteFileEvent>,
    pub cancellation: WorkerCancellation,
}

impl SvnCertificateContext {
    /// 在 TLS 握手已失败并释放后通知 UI；等待有界，拒绝不会触发网络重试。
    pub(super) fn confirm(&self, prompt: SvnCertificatePrompt) -> Result<()> {
        self.confirm_with_timeout(prompt, SVN_CERTIFICATE_APPROVAL_TIMEOUT)
    }

    fn confirm_with_timeout(&self, prompt: SvnCertificatePrompt, timeout: Duration) -> Result<()> {
        self.cancellation.ensure_active()?;
        let (request, receiver) = SvnCertificateApproval::new(prompt, self.cancellation.clone());
        let _pending_guard = PendingApprovalGuard(request.pending.clone());
        self.event_sender
            .try_send(RemoteFileEvent::SvnCertificateVerification {
                session_id: self.session_id,
                request,
            })
            .context("无法显示 SVN 证书确认请求")?;
        let started = Instant::now();
        loop {
            self.cancellation.ensure_active()?;
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                bail!("等待 SVN 证书确认超时，请重新连接");
            }
            match receiver.recv_timeout(remaining.min(SVN_CERTIFICATE_CANCEL_INTERVAL)) {
                Ok(SvnCertificateDecision::Reject) => bail!("已拒绝 SVN 服务器证书"),
                Ok(
                    SvnCertificateDecision::TrustOnce | SvnCertificateDecision::TrustPermanently,
                ) => {
                    self.cancellation.ensure_active()?;
                    return Ok(());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => bail!("SVN 证书确认窗口已关闭"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::svn_tls::SvnCertificateFailure;
    use super::super::worker::WorkerGuard;
    use super::*;

    fn prompt() -> SvnCertificatePrompt {
        SvnCertificatePrompt {
            host: "svn.example.test".into(),
            port: 443,
            fingerprint: format!("SHA256:{}", "ab".repeat(32)),
            subject: "CN=svn.example.test".into(),
            issuer: "CN=测试签发机构".into(),
            subject_alt_names: vec!["svn.example.test".into()],
            not_before: "2026-01-01".into(),
            not_after: "2027-01-01".into(),
            failures: vec![SvnCertificateFailure::UnknownIssuer],
            previous_fingerprint: None,
        }
    }

    #[test]
    fn approvals_are_single_use_and_decisions_reach_only_their_request() {
        for decision in [
            SvnCertificateDecision::TrustOnce,
            SvnCertificateDecision::TrustPermanently,
            SvnCertificateDecision::Reject,
        ] {
            let (request, receiver) = SvnCertificateApproval::test_request(prompt());
            let (other, _) = SvnCertificateApproval::test_request(prompt());
            assert_ne!(request.request_id, other.request_id);
            assert!(request.is_pending());
            request.respond(decision).unwrap();
            assert_eq!(receiver.recv().unwrap(), decision);
            assert!(!request.is_pending());
            assert!(request.respond(decision).is_err());
            assert!(other.is_pending());
        }
    }

    #[test]
    fn approval_wait_handles_accept_reject_cancel_and_timeout() {
        for decision in [
            Some(SvnCertificateDecision::TrustOnce),
            Some(SvnCertificateDecision::Reject),
            None,
        ] {
            let (guard, cancellation) = WorkerGuard::new(9);
            let (sender, receiver) = async_channel::unbounded();
            let context = SvnCertificateContext {
                session_id: 9,
                event_sender: sender,
                cancellation,
            };
            let worker = std::thread::spawn(move || context.confirm(prompt()));
            let RemoteFileEvent::SvnCertificateVerification {
                session_id,
                request,
            } = receiver.recv_blocking().unwrap()
            else {
                panic!("应收到证书确认事件")
            };
            assert_eq!(session_id, 9);
            if let Some(decision) = decision {
                request.respond(decision).unwrap();
            }
            if decision.is_none() {
                drop(guard);
            }
            let result = worker.join().unwrap();
            assert_eq!(
                result.is_ok(),
                decision == Some(SvnCertificateDecision::TrustOnce)
            );
            assert!(!request.is_pending());
        }
        let (sender, receiver) = async_channel::unbounded();
        let context = SvnCertificateContext {
            session_id: 1,
            event_sender: sender,
            cancellation: WorkerCancellation::default(),
        };
        assert!(
            context
                .confirm_with_timeout(prompt(), Duration::ZERO)
                .unwrap_err()
                .to_string()
                .contains("超时")
        );
        let RemoteFileEvent::SvnCertificateVerification { request, .. } =
            receiver.recv_blocking().unwrap()
        else {
            panic!("应收到证书确认事件")
        };
        assert!(!request.is_pending());
        assert!(request.respond(SvnCertificateDecision::TrustOnce).is_err());
    }

    #[test]
    fn closed_ui_channel_fails_without_waiting() {
        let (sender, receiver) = async_channel::unbounded();
        drop(receiver);
        let context = SvnCertificateContext {
            session_id: 1,
            event_sender: sender,
            cancellation: WorkerCancellation::default(),
        };
        assert!(
            context
                .confirm(prompt())
                .unwrap_err()
                .to_string()
                .contains("无法显示")
        );
    }
}
