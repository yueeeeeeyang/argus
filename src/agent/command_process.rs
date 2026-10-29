//! 文件职责：有界收集命令输出，并在完成、取消或超时时终止和回收整个命令进程组。

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

/// 输出只保留固定预算，超额字节继续排空，避免子进程因管道写满阻塞。
pub(super) struct CapturedOutput {
    pub bytes: Vec<u8>,
    pub truncated: bool,
}

/// 命令退出结果；保留非零退出码，由模型和界面如实判断执行是否成功。
pub(super) struct CommandOutput {
    pub exit_code: Option<i32>,
    pub stdout: CapturedOutput,
    pub stderr: CapturedOutput,
}

/// 独立任务拥有子进程的清理责任，调用方 future 被丢弃时仍可完成异步 wait 回收。
pub(super) async fn execute_command(
    command: Command,
    timeout: Duration,
    cancellation: &CancellationToken,
    stdout_limit: usize,
    stderr_limit: usize,
) -> Result<CommandOutput, String> {
    let cancellation = cancellation.child_token();
    let guard = CancelOnDrop(cancellation.clone());
    let task = tokio::spawn(run_command(
        command,
        timeout,
        cancellation,
        stdout_limit,
        stderr_limit,
    ));
    let result = task
        .await
        .map_err(|error| format!("command worker failed: {error}"))?;
    drop(guard);
    result
}

/// 调用者退出只发取消，不 abort worker；worker 必须继续终止并回收子进程。
struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// 管理独立进程树；所有终态都先清理树，再等待主进程退出。
async fn run_command(
    mut command: Command,
    timeout: Duration,
    cancellation: CancellationToken,
    stdout_limit: usize,
    stderr_limit: usize,
) -> Result<CommandOutput, String> {
    if cancellation.is_cancelled() {
        return Err("command cancelled before execution".to_string());
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    ProcessTree::configure(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot spawn command: {error}"))?;
    let mut tree = match ProcessTree::attach(&child) {
        Ok(tree) => tree,
        Err(error) => {
            return match child.kill().await {
                Ok(()) => Err(format!("cannot isolate command process tree: {error}")),
                Err(cleanup) => Err(format!(
                    "cannot isolate command process tree: {error}; cannot reap child: {cleanup}"
                )),
            };
        }
    };
    let stdout = child.stdout.take().ok_or("command stdout is unavailable")?;
    let stderr = child.stderr.take().ok_or("command stderr is unavailable")?;
    let result = tokio::select! {
        _ = cancellation.cancelled() => Err("command cancelled".to_string()),
        _ = tokio::time::sleep(timeout) => Err(format!("command timed out after {} ms", timeout.as_millis())),
        output = async {
            let (status, stdout, stderr) = tokio::try_join!(
                async {
                    let status = child.wait().await?;
                    // 主进程退出立即清理后代，不在已回收 PID 上等待输出排空后再发信号。
                    tree.terminate()?;
                    Ok::<_, std::io::Error>(status)
                },
                capture_output(stdout, stdout_limit),
                capture_output(stderr, stderr_limit),
            )?;
            Ok::<_, std::io::Error>(CommandOutput { exit_code: status.code(), stdout, stderr })
        } => output.map_err(|error| format!("cannot read command output: {error}")),
    };
    // 即使 shell 已退出，也可能留下关闭了管道的后台子进程，完成路径同样清理整个组。
    tree.terminate()
        .map_err(|error| format!("cannot terminate command process tree: {error}"))?;
    child
        .wait()
        .await
        .map_err(|error| format!("cannot reap command process: {error}"))?;
    result
}

/// 持续排空异步字节流，但保留的字节永远不超过 limit；读取失败必须上报。
async fn capture_output(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
) -> std::io::Result<CapturedOutput> {
    let mut captured = CapturedOutput {
        bytes: Vec::with_capacity(limit.min(8192)),
        truncated: false,
    };
    let mut chunk = [0_u8; 8192];
    loop {
        let count = reader.read(&mut chunk).await?;
        if count == 0 {
            return Ok(captured);
        }
        let retained = count.min(limit.saturating_sub(captured.bytes.len()));
        captured.bytes.extend_from_slice(&chunk[..retained]);
        captured.truncated |= retained < count;
    }
}

#[cfg(unix)]
struct ProcessTree {
    group_id: i32,
    is_active: bool,
}

#[cfg(unix)]
impl ProcessTree {
    /// Unix 子进程自建进程组，后续取消能覆盖管道和普通派生进程。
    fn configure(command: &mut Command) {
        command.process_group(0);
    }

    /// 保存进程组标识；没有可用 PID 时返回错误，不继续执行未受控命令。
    fn attach(child: &tokio::process::Child) -> std::io::Result<Self> {
        let group_id = child
            .id()
            .ok_or_else(|| std::io::Error::other("child PID is unavailable"))?
            as i32;
        Ok(Self {
            group_id,
            is_active: true,
        })
    }

    /// 向整个组发送终止信号；组已消失视为完成，其余错误必须返回。
    fn terminate(&mut self) -> std::io::Result<()> {
        if !self.is_active {
            return Ok(());
        }
        // 子进程拥有独立进程组；负 PID 只向该组发信号，不会杀死 Argus 自身所在组。
        let result = unsafe { libc::kill(-self.group_id, libc::SIGKILL) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        self.is_active = false;
        Ok(())
    }
}

#[cfg(windows)]
struct ProcessTree {
    job: std::os::windows::io::OwnedHandle,
    process_id: u32,
    is_active: bool,
}

#[cfg(windows)]
impl ProcessTree {
    /// 在主线程开始前挂起进程，为 Job 归组保留无竞态窗口。
    fn configure(command: &mut Command) {
        // 先挂起主线程，加入 Job 后再恢复，防止程序在归组之前派生逃逸子进程。
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
    }

    /// 创建退出即清理的 Job、加入挂起子进程并恢复主线程；任何一步失败均拒绝运行。
    fn attach(child: &tokio::process::Child) -> std::io::Result<Self> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };
        // 所有句柄转交 OwnedHandle；配置失败也会关闭 Job 并终止已归组的挂起进程。
        let raw_job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw_job.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let job = unsafe { OwnedHandle::from_raw_handle(raw_job.cast()) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                job.as_raw_handle().cast(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if configured == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let process = child
            .raw_handle()
            .ok_or_else(|| std::io::Error::other("child handle is unavailable"))?;
        if unsafe { AssignProcessToJobObject(job.as_raw_handle().cast(), process.cast()) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let process_id = child
            .id()
            .ok_or_else(|| std::io::Error::other("child PID is unavailable"))?;
        let tree = Self {
            job,
            process_id,
            is_active: true,
        };
        resume_suspended_process(process_id)?;
        Ok(tree)
    }

    /// 终止 Job 中全部进程；失败返回操作系统错误，不能把主进程退出冒充树清理成功。
    fn terminate(&mut self) -> std::io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        if !self.is_active {
            return Ok(());
        }
        // Job 包括后续派生进程；不能只终止顶层 bash。
        if unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(
                self.job.as_raw_handle().cast(),
                1,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        self.is_active = false;
        Ok(())
    }
}

/// 标准进程接口不暴露初始线程句柄，按挂起子进程 PID 找到主线程后恢复。
#[cfg(windows)]
fn resume_suspended_process(process_id: u32) -> std::io::Result<()> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};
    // 快照和线程句柄均由 OwnedHandle 在所有成功/失败路径释放。
    let raw_snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if raw_snapshot == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw_snapshot.cast()) };
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut found = unsafe { Thread32First(snapshot.as_raw_handle().cast(), &mut entry) };
    while found != 0 {
        if entry.th32OwnerProcessID == process_id {
            let raw_thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if raw_thread.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let thread = unsafe { OwnedHandle::from_raw_handle(raw_thread.cast()) };
            if unsafe { ResumeThread(thread.as_raw_handle().cast()) } == u32::MAX {
                return Err(std::io::Error::last_os_error());
            }
            return Ok(());
        }
        found = unsafe { Thread32Next(snapshot.as_raw_handle().cast(), &mut entry) };
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
        Err(std::io::Error::other(
            "suspended child thread was not found",
        ))
    } else {
        Err(error)
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        if let Err(error) = self.terminate() {
            #[cfg(unix)]
            let process_id = self.group_id;
            #[cfg(windows)]
            let process_id = self.process_id;
            eprintln!("终止智能助手命令进程树失败，进程 ID={process_id}：{error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 输出超过预算后继续消费，但内存保留量不随输出大小增长。
    #[tokio::test]
    async fn output_capture_is_bounded_and_marks_truncation() {
        let source = vec![b'x'; 128 * 1024];
        let output = capture_output(source.as_slice(), 1024)
            .await
            .expect("应读取输出");
        assert_eq!(output.bytes.len(), 1024);
        assert!(output.truncated);
        let exact = capture_output(&source[..1024], 1024)
            .await
            .expect("应读取边界输出");
        assert!(!exact.truncated);
    }

    /// 超时不仅杀 shell，仍在等待中的后代也必须被终止，不能稍后继续写文件。
    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_and_cancellation_terminate_descendants() {
        for cancel in [false, true] {
            let workspace = tempfile::tempdir().expect("应创建测试目录");
            let marker = workspace.path().join("survived");
            let mut command = Command::new("/bin/bash");
            command
                .current_dir(workspace.path())
                .args(["-c", "bash -c 'sleep 0.4; echo survived > survived' & wait"]);
            let cancellation = CancellationToken::new();
            let cancelling = cancellation.clone();
            let task = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                if cancel {
                    cancelling.cancel();
                }
            });
            let result =
                execute_command(command, Duration::from_millis(100), &cancellation, 128, 128).await;
            assert!(result.is_err());
            task.await.expect("取消任务应结束");
            tokio::time::sleep(Duration::from_millis(500)).await;
            assert!(!marker.exists(), "结束后的子进程不得继续写文件");
        }
    }

    /// 上层流消费者直接丢弃工具 future 时，独立 worker 仍负责终止并回收后代。
    #[cfg(unix)]
    #[tokio::test]
    async fn dropped_caller_still_cleans_up_descendants() {
        let workspace = tempfile::tempdir().expect("应创建测试目录");
        let marker = workspace.path().join("survived");
        let ready = workspace.path().join("ready");
        let mut command = Command::new("/bin/bash");
        command.current_dir(workspace.path()).args([
            "-c",
            "bash -c 'echo ready > ready; sleep 0.4; echo survived > survived' & wait",
        ]);
        let caller = tokio::spawn(async move {
            execute_command(
                command,
                Duration::from_secs(5),
                &CancellationToken::new(),
                128,
                128,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !ready.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("子进程应进入等待阶段");
        caller.abort();
        assert!(matches!(caller.await, Err(error) if error.is_cancelled()));
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!marker.exists(), "调用方丢弃后不能残留仍运行的子进程");
    }

    /// 启动失败和非零退出必须保持可见；正常输出也经过相同预算。
    #[cfg(unix)]
    #[tokio::test]
    async fn command_exit_and_spawn_errors_remain_visible() {
        let mut command = Command::new("/bin/bash");
        command.args(["-c", "printf out; printf error >&2; exit 7"]);
        let output = execute_command(
            command,
            Duration::from_secs(2),
            &CancellationToken::new(),
            2,
            2,
        )
        .await
        .expect("应获取退出状态");
        assert_eq!(output.exit_code, Some(7));
        assert_eq!(output.stdout.bytes, b"ou");
        assert!(output.stdout.truncated && output.stderr.truncated);
        let result = execute_command(
            Command::new("/nonexistent/argus-test-command"),
            Duration::from_secs(1),
            &CancellationToken::new(),
            2,
            2,
        )
        .await;
        assert!(result.is_err());
    }

    /// Windows 挂起进程加入 Job 后仍应正常恢复，并返回退出码和有界输出。
    #[cfg(windows)]
    #[tokio::test]
    async fn windows_job_resumes_and_preserves_exit_status() {
        let mut command = Command::new("cmd.exe");
        command.args(["/D", "/C", "echo output & exit /B 7"]);
        let output = execute_command(
            command,
            Duration::from_secs(5),
            &CancellationToken::new(),
            3,
            3,
        )
        .await
        .expect("Job 内的命令应可恢复执行");
        assert_eq!(output.exit_code, Some(7));
        assert_eq!(output.stdout.bytes, b"out");
        assert!(output.stdout.truncated);
    }

    /// Windows 在 Job 分配前保持挂起，取消后派生 cmd 也不得继续执行批处理。
    #[cfg(windows)]
    #[tokio::test]
    async fn windows_job_cancellation_terminates_descendants() {
        let workspace = tempfile::tempdir().expect("应创建测试目录");
        std::fs::write(workspace.path().join("child.cmd"),
            "@echo off\r\necho ready > ready\r\nping -n 4 127.0.0.1 > nul\r\necho survived > survived\r\n")
            .expect("应创建子进程批处理");
        let mut command = Command::new("cmd.exe");
        command.current_dir(workspace.path()).args([
            "/D",
            "/C",
            "start /B /WAIT cmd.exe /D /C child.cmd",
        ]);
        let cancellation = CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let worker = tokio::spawn(async move {
            execute_command(
                command,
                Duration::from_secs(10),
                &worker_cancellation,
                128,
                128,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !workspace.path().join("ready").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("子进程应已启动");
        cancellation.cancel();
        assert!(worker.await.expect("命令任务应已回收").is_err());
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(!workspace.path().join("survived").exists());
    }
}
