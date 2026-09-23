//! 文件职责：把用户选择的日志来源物化到独立工作目录。
//! 创建日期：2026-09-12
//! 作者：Argus 开发团队
//! 主要功能：加载日志时将普通文件/目录复制、压缩包（含嵌套）解压到 `cache/workdirs/<id>/`，
//! 统一浏览器与 AI 的读取模型为普通文件；提供条目路径清洗和残留工作目录清扫。

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use tokio_util::sync::CancellationToken;

use crate::config::LoaderConfig;
use crate::config::paths::argus_config_dir;
use crate::loader::archive::adapter::{ArchiveAdapter, ArchiveEntryConsumer, ArchiveEntrySession};
use crate::loader::archive::detector::{
    ArchiveFormat, detect_archive_format, detect_archive_format_by_name,
};
use crate::loader::archive::password::{
    ArchivePasswordError, ArchivePasswordErrorKind, ArchivePasswordKey, ArchivePasswordStore,
};
use crate::loader::archive::registry::archive_registry;
use crate::loader::source_scanner::{SourceLoadPhase, SourceTreeScanProgress};

/// Argus 缓存目录名。
const ARGUS_CACHE_DIR_NAME: &str = "cache";
/// 物化工作目录根目录名。
const WORKSPACES_DIR_NAME: &str = "workdirs";
/// 复制/解压写入缓冲区大小。
const COPY_BUFFER_BYTES: usize = 64 * 1024;
/// 物化进度上报条目间隔，避免高频进度淹没通道。
const PROGRESS_REPORT_INTERVAL: usize = 32;
/// 并行解压普通条目的最小任务数；任务过少时线程调度开销大于收益。
const PARALLEL_EXTRACT_MIN_JOBS: usize = 8;
/// 并行解压的 worker 上限；磁盘写入带宽是共享资源，worker 过多反而互相争抢。
const PARALLEL_EXTRACT_MAX_WORKERS: usize = 10;
/// 允许并行的最大嵌套层级；更深层保持顺序，避免线程池随嵌套层级成倍扩张。
const PARALLEL_EXTRACT_MAX_DEPTH: usize = 1;
/// 删除工作目录失败时的后台重试次数，规避 Windows 句柄延迟释放。
const DELETE_RETRY_COUNT: usize = 5;
/// 删除工作目录失败时的重试间隔（毫秒）。
const DELETE_RETRY_INTERVAL_MS: u64 = 500;

/// 一次完整物化的产物；随来源替换而失效。
#[derive(Debug)]
pub(crate) struct MaterializedWorkspace {
    /// 工作目录根路径。
    pub root: PathBuf,
    /// 物化成功的顶层根（用于来源树扫描）。
    pub roots: Vec<MaterializedRootInfo>,
    /// 因密码未授权而跳过的最外层压缩包（用于来源树密码占位节点）。
    pub password_pending: Vec<PasswordPendingArchive>,
    /// 物化期间的非致命警告（跳过条目、冲突等）。
    pub warnings: Vec<String>,
    /// 已物化文件数量。
    pub materialized_files: usize,
}

/// 物化成功的顶层根信息。
#[derive(Clone, Debug)]
pub(crate) struct MaterializedRootInfo {
    /// 顶层目录真实路径。
    pub path: PathBuf,
}

/// 因密码未授权而跳过的最外层压缩包；解锁后可向工作目录追加物化。
#[derive(Clone, Debug)]
pub(crate) struct PasswordPendingArchive {
    /// 压缩包真实路径。
    pub archive_path: PathBuf,
    /// 占位展示标签（原文件名去压缩扩展名）。
    pub label: String,
}

/// 物化进度通道；后台物化任务与来源扫描共用同一进度模型。
type MaterializeProgress<'a> = Option<&'a std::sync::mpsc::Sender<SourceTreeScanProgress>>;

/// 单个来源根物化的失败分类；取消和致命错误终止整个加载。
#[derive(Debug)]
enum MaterializeRootError {
    /// 最外层压缩包需要密码；该根回滚并转为密码占位，解锁后可追加物化。
    PasswordPending,
    /// 加载被新请求取消，整个物化必须立即终止。
    Cancelled,
    /// 其它不可恢复错误（I/O、工作目录创建失败等）。
    Fatal(anyhow::Error),
}

impl std::fmt::Display for MaterializeRootError {
    /// 输出用户可读的物化失败原因。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PasswordPending => write!(f, "压缩包需要密码"),
            Self::Cancelled => write!(f, "已取消日志加载"),
            Self::Fatal(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for MaterializeRootError {}

/// 正在解压的压缩容器上下文；嵌套容器先落盘 scratch 后复用同一解压路径。
struct ArchiveContainerContext {
    /// 当前容器文件路径（根为真实压缩包，嵌套为 scratch 落盘文件）。
    path: PathBuf,
    /// 用户可见的容器展示名；嵌套容器保留"外层包!/内层包"链路，避免暴露 scratch 路径。
    display: String,
    /// 当前容器格式。
    format: ArchiveFormat,
    /// 最外层真实压缩包路径，用于密码查询。
    root_archive: PathBuf,
}

/// 普通文件条目的解压任务；条目之间互不依赖，可交给 worker 线程池并行执行。
struct PlainEntryJob {
    /// 压缩包内条目路径。
    entry_path: String,
    /// 落盘目标路径。
    target: PathBuf,
    /// 用户可见的条目展示名。
    display_entry: String,
}

/// 嵌套条目的解压任务；同样彼此独立，可交给 worker 线程池并行执行。
#[derive(Clone)]
enum NestedEntryJob {
    /// 嵌套单文件 gzip。
    Gzip {
        /// 压缩包内条目路径。
        entry_path: String,
        /// 落盘目标路径。
        target: PathBuf,
        /// 用户可见的条目展示名。
        display_entry: String,
    },
    /// 嵌套压缩容器。
    Container {
        /// 压缩包内条目路径。
        entry_path: String,
        /// 嵌套容器格式。
        format: ArchiveFormat,
        /// 落盘目标路径。
        target: PathBuf,
        /// 用户可见的条目展示名。
        display_entry: String,
    },
}

/// 并行嵌套解压 worker 的实际产出，供主线程合并文件计数与警告。
#[derive(Default)]
struct NestedWorkerOutcome {
    /// 该 worker 实际写盘的文件数。
    files_written: usize,
    /// 该 worker 产生的非致命警告。
    warnings: Vec<String>,
}

/// 嵌套 scratch 文件名的全局序号；并行解压时保证不同 worker 不会重名。
static NEXT_SCRATCH_SEQUENCE: AtomicUsize = AtomicUsize::new(0);
/// 嵌套单文件提升到父目录时的全局互斥；避免并行 worker 重名竞争导致覆盖。
static NESTED_PROMOTE_LOCK: Mutex<()> = Mutex::new(());

/// 生成唯一嵌套 scratch 文件路径。
fn next_scratch_file(root: &Path, depth: usize) -> PathBuf {
    let sequence = NEXT_SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    root.join(".argus-scratch")
        .join(format!("nested-{sequence}-{depth}"))
}

/// 并行解压 worker 的独立会话工厂；只对支持会话式读取的格式可用。
struct SessionFactory<'a> {
    /// 当前容器格式适配器。
    adapter: &'a dyn ArchiveAdapter,
    /// 容器文件路径。
    path: &'a Path,
    /// 解密密码。
    password: Option<&'a str>,
}

impl SessionFactory<'_> {
    /// 新建一个独立会话；格式不支持或打开失败时返回 `None`，调用方退回顺序解压。
    fn open(&self) -> Option<Box<dyn ArchiveEntrySession>> {
        self.adapter
            .open_session(self.path, self.password)
            .ok()
            .flatten()
    }
}

/// 解压过程中的条目读取来源。
///
/// 会话优先：一次打开压缩包复用已解析的中央目录，条目数量大的包不再逐条目
/// 重新打开；不支持会话的格式回退适配器逐条目路径，语义完全一致。
enum EntrySource<'a> {
    /// 一次打开的会话句柄。
    Session(Box<dyn ArchiveEntrySession>),
    /// 逐条目打开的适配器路径。
    Adapter {
        /// 格式适配器。
        adapter: &'a dyn ArchiveAdapter,
        /// 压缩包路径。
        path: &'a Path,
        /// 解密密码。
        password: Option<&'a str>,
    },
}

impl EntrySource<'_> {
    /// 枚举全部条目。
    fn list_entries(
        &mut self,
    ) -> anyhow::Result<Vec<crate::loader::archive::adapter::ArchiveEntryInfo>> {
        match self {
            Self::Session(session) => session.list_entries(),
            Self::Adapter {
                adapter,
                path,
                password,
            } => adapter.list_entries(path, *password),
        }
    }

    /// 流式输出一个条目。
    fn stream_entry(
        &mut self,
        entry_path: &str,
        consumer: &mut ArchiveEntryConsumer<'_>,
    ) -> anyhow::Result<()> {
        match self {
            Self::Session(session) => session.stream_entry(entry_path, consumer),
            Self::Adapter {
                adapter,
                path,
                password,
            } => adapter.stream_entry(path, entry_path, *password, consumer),
        }
    }
}

/// 物化器：把每个来源根复制或解压为工作目录下的一个顶层目录。
struct WorkspaceMaterializer<'a> {
    /// 目录、符号链接和嵌套深度配置。
    config: &'a LoaderConfig,
    /// 当前进程已授权的归档密码快照。
    passwords: &'a ArchivePasswordStore,
    /// 新加载请求到来时中断物化的取消令牌。
    cancellation: &'a CancellationToken,
    /// 物化进度上报通道。
    progress: MaterializeProgress<'a>,
    /// 工作目录根。
    root: PathBuf,
    /// 已使用的顶层目录名（小写），避免不同来源根互相覆盖。
    used_labels: HashSet<String>,
    /// 已物化文件计数，用于进度展示。
    files_written: usize,
    /// 物化成功的顶层根。
    roots: Vec<MaterializedRootInfo>,
    /// 因密码未授权而跳过的最外层压缩包。
    password_pending: Vec<PasswordPendingArchive>,
    /// 非致命警告集合。
    warnings: Vec<String>,
}

/// 把全部来源路径物化到一个新的工作目录；任何致命错误都会回滚整个工作目录。
///
/// 参数说明：
/// - `paths`：用户选择的本地文件、目录或压缩包路径。
/// - `config`：目录、符号链接和嵌套深度配置。
/// - `archive_passwords`：当前进程已授权的归档密码快照。
/// - `cancellation`：新加载请求到来时中断物化的取消令牌。
/// - `progress`：可选进度通道，与来源扫描共用同一进度模型。
///
/// 返回值：物化成功时返回工作目录；被取消或发生致命错误时返回 `Err` 且不留残留目录。
pub(crate) fn materialize_sources(
    paths: &[PathBuf],
    config: &LoaderConfig,
    archive_passwords: &ArchivePasswordStore,
    cancellation: &CancellationToken,
    progress: MaterializeProgress<'_>,
) -> Result<MaterializedWorkspace> {
    let root = unique_workspace_root();
    fs::create_dir_all(&root)
        .with_context(|| format!("无法创建日志工作目录：{}", root.display()))?;

    let mut materializer = WorkspaceMaterializer {
        config,
        passwords: archive_passwords,
        cancellation,
        progress,
        root: root.clone(),
        used_labels: HashSet::new(),
        files_written: 0,
        roots: Vec::new(),
        password_pending: Vec::new(),
        warnings: Vec::new(),
    };

    if let Err(error) = materializer.materialize_all(paths) {
        let _ = fs::remove_dir_all(&root);
        return Err(error);
    }

    // 嵌套解压的 scratch 暂存目录不留在最终产物中。
    let _ = fs::remove_dir_all(root.join(".argus-scratch"));

    Ok(MaterializedWorkspace {
        root,
        roots: materializer.roots,
        password_pending: materializer.password_pending,
        warnings: materializer.warnings,
        materialized_files: materializer.files_written,
    })
}

/// 返回物化工作目录的根目录。
pub(crate) fn workspaces_dir() -> PathBuf {
    argus_config_dir()
        .join(ARGUS_CACHE_DIR_NAME)
        .join(WORKSPACES_DIR_NAME)
}

/// 后台尽力删除一个工作目录；失败时按固定间隔重试，规避 Windows 句柄延迟释放。
///
/// 说明：替换、退出或回滚场景都允许"稍后删除成功"，最终一致性由启动清扫兜底。
pub(crate) fn delete_workspace_best_effort(root: PathBuf) {
    std::thread::Builder::new()
        .name("argus-workspace-delete".to_string())
        .spawn(move || {
            for attempt in 0..DELETE_RETRY_COUNT {
                if !root.exists() {
                    return;
                }
                match fs::remove_dir_all(&root) {
                    Ok(()) => return,
                    Err(error) if attempt + 1 < DELETE_RETRY_COUNT => {
                        eprintln!(
                            "删除日志工作目录失败（第 {} 次），稍后重试：{}：{}",
                            attempt + 1,
                            root.display(),
                            error
                        );
                        std::thread::sleep(std::time::Duration::from_millis(
                            DELETE_RETRY_INTERVAL_MS,
                        ));
                    }
                    Err(error) => {
                        eprintln!(
                            "删除日志工作目录最终失败，留待启动清扫：{}：{}",
                            root.display(),
                            error
                        );
                    }
                }
            }
        })
        .map(|_| ())
        .unwrap_or_else(|error| {
            eprintln!("无法启动日志工作目录删除线程：{error}");
        });
}

/// 解锁后向既有工作目录追加物化一个压缩包。
///
/// 参数说明：
/// - `workspace_root`：当前工作目录根。
/// - `preferred_label`：期望的顶层目录名（通常取占位节点标签）；冲突时自动消歧。
/// - `archive_path`：已解锁的压缩包真实路径。
/// - `config`：嵌套深度配置。
/// - `archive_passwords`：当前进程已授权的归档密码快照。
/// - `cancellation`：中断追加物化的取消令牌。
///
/// 返回值：追加成功的顶层目录；密码错误或 I/O 失败时返回 `Err` 并回滚半成品。
pub(crate) fn append_materialize_archive(
    workspace_root: &Path,
    preferred_label: &str,
    archive_path: &Path,
    config: &LoaderConfig,
    archive_passwords: &ArchivePasswordStore,
    cancellation: &CancellationToken,
) -> Result<MaterializedRootInfo> {
    let format = detect_archive_format(archive_path)
        .filter(|format| format.is_supported())
        .with_context(|| format!("无法识别压缩包格式：{}", archive_path.display()))?;
    let label = unique_label_in_dir(workspace_root, preferred_label);
    let label_dir = workspace_root.join(&label);

    let mut materializer = WorkspaceMaterializer {
        config,
        passwords: archive_passwords,
        cancellation,
        progress: None,
        root: workspace_root.to_path_buf(),
        used_labels: HashSet::new(),
        files_written: 0,
        roots: Vec::new(),
        password_pending: Vec::new(),
        warnings: Vec::new(),
    };
    let container = ArchiveContainerContext {
        path: archive_path.to_path_buf(),
        display: archive_path.display().to_string(),
        format,
        root_archive: archive_path.to_path_buf(),
    };
    let result = materializer.extract_archive(&container, &label_dir, Vec::new(), 0);
    let _ = fs::remove_dir_all(workspace_root.join(".argus-scratch"));
    match result {
        Ok(()) => {
            let root_path = materializer.promote_single_file_root(&label, &label_dir);
            Ok(MaterializedRootInfo { path: root_path })
        }
        Err(MaterializeRootError::PasswordPending) => {
            let _ = fs::remove_dir_all(&label_dir);
            // 追加物化前用户刚输入过密码，仍失败即密码错误；保留密码错误类型供界面再次弹窗。
            Err(
                ArchivePasswordError::invalid(archive_path.display().to_string())
                    .with_context(
                        ArchivePasswordKey::root(archive_path.to_path_buf()),
                        archive_path.display().to_string(),
                    )
                    .into(),
            )
        }
        Err(MaterializeRootError::Cancelled) => {
            let _ = fs::remove_dir_all(&label_dir);
            bail!("已取消")
        }
        Err(MaterializeRootError::Fatal(error)) => {
            let _ = fs::remove_dir_all(&label_dir);
            Err(error)
        }
    }
}

/// 在既有工作目录中生成不冲突的顶层目录名。
fn unique_label_in_dir(workspace_root: &Path, preferred: &str) -> String {
    let sanitized = sanitize_label(preferred);
    let mut candidate = sanitized.clone();
    let mut suffix = 1_usize;
    while workspace_root.join(&candidate).exists() {
        suffix += 1;
        candidate = format!("{sanitized} ({suffix})");
    }
    candidate
}

/// 判断错误是否为可重试的压缩包密码错误（缺少密码或密码错误）；
/// 底层算法不支持的加密不属于此类，重试无意义。
fn is_retryable_password_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<ArchivePasswordError>()
        .is_some_and(|password_error| {
            matches!(
                password_error.kind,
                ArchivePasswordErrorKind::Required | ArchivePasswordErrorKind::Invalid
            )
        })
}

/// 把条目流式解压到指定路径；不计数、不上报进度，供普通条目与嵌套容器 scratch 共用。
///
/// 说明：该函数不访问物化器状态，可由并行解压 worker 直接调用；失败时删除半成品文件。
fn stream_entry_to_path(
    source: &mut EntrySource<'_>,
    entry_path: &str,
    target: &Path,
    display_entry: &str,
) -> Result<(), MaterializeRootError> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            MaterializeRootError::Fatal(
                anyhow::Error::new(error)
                    .context(format!("无法创建物化目录：{}", parent.display())),
            )
        })?;
    }
    let mut file = File::create(target).map_err(|error| {
        MaterializeRootError::Fatal(
            anyhow::Error::new(error).context(format!("无法创建物化文件：{}", target.display())),
        )
    })?;

    let consume_result = source.stream_entry(entry_path, &mut |chunk: &[u8]| {
        file.write_all(chunk)?;
        Ok(())
    });
    consume_result.map_err(|error| {
        let _ = fs::remove_file(target);
        if is_retryable_password_error(&error) {
            return MaterializeRootError::PasswordPending;
        }
        MaterializeRootError::Fatal(error.context(format!("解压条目失败：{display_entry}")))
    })
}

/// 根据任务数与 CPU 数决定并行解压 worker 数量。
fn parallel_worker_count(job_count: usize) -> usize {
    let available = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(PARALLEL_EXTRACT_MAX_WORKERS);
    job_count.min(available).max(1)
}

/// 记录并行解压的失败原因；多个 worker 同时失败时保留先写入的那个。
fn store_parallel_failure(slot: &Mutex<Option<MaterializeRootError>>, error: MaterializeRootError) {
    if let Ok(mut guard) = slot.lock()
        && guard.is_none()
    {
        *guard = Some(error);
    }
}

/// 启动时清扫全部残留工作目录和历史压缩分页缓存（崩溃或强杀后的兜底清理）。
pub(crate) fn sweep_stale_workspaces() {
    let workspaces = workspaces_dir();
    if workspaces.exists()
        && let Err(error) = fs::remove_dir_all(&workspaces)
    {
        eprintln!(
            "启动清扫残留日志工作目录失败：{}：{error}",
            workspaces.display()
        );
    }

    let legacy_log_pages = argus_config_dir()
        .join(ARGUS_CACHE_DIR_NAME)
        .join("log_pages");
    if legacy_log_pages.exists()
        && let Err(error) = fs::remove_dir_all(&legacy_log_pages)
    {
        eprintln!(
            "启动清扫历史日志分页缓存失败：{}：{error}",
            legacy_log_pages.display()
        );
    }
}

/// 生成唯一工作目录根路径（不创建目录）。
fn unique_workspace_root() -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    workspaces_dir().join(format!("argus-{}-{timestamp}", std::process::id()))
}

impl WorkspaceMaterializer<'_> {
    /// 逐个物化来源根；取消和致命错误上抛。
    fn materialize_all(&mut self, paths: &[PathBuf]) -> Result<()> {
        for path in paths {
            self.ensure_not_cancelled()?;
            self.report_progress(&display_path_for_progress(path));
            if let Err(error) = self.materialize_root(path) {
                match error {
                    // 密码占位已在 materialize_root 内部消化，不会传播到这里。
                    MaterializeRootError::PasswordPending => unreachable!("密码占位不应上抛"),
                    MaterializeRootError::Cancelled => bail!("已取消日志加载"),
                    MaterializeRootError::Fatal(error) => return Err(error),
                }
            }
        }
        Ok(())
    }

    /// 物化单个来源根为一个顶层目录；失败回滚该根的半成品，密码未授权转为密码占位。
    ///
    /// 可展开压缩包若只包含一个文件，产物直接提升为工作目录顶层的文件根，
    /// 不再套一层标签目录。
    fn materialize_root(&mut self, path: &Path) -> Result<(), MaterializeRootError> {
        let base_label = display_label_for_path(path);
        let label = self.unique_top_label(&base_label);
        let label_dir = self.root.join(&label);
        let is_extractable_archive =
            detect_archive_format(path).is_some_and(|format| format.is_supported());
        match self.materialize_root_into(path, &label_dir) {
            Ok(()) => {
                let root_path = if is_extractable_archive {
                    self.promote_single_file_root(&label, &label_dir)
                } else {
                    label_dir
                };
                self.roots.push(MaterializedRootInfo { path: root_path });
                Ok(())
            }
            Err(MaterializeRootError::PasswordPending) => {
                let _ = fs::remove_dir_all(&label_dir);
                self.password_pending.push(PasswordPendingArchive {
                    archive_path: path.to_path_buf(),
                    label,
                });
                Ok(())
            }
            Err(error) => {
                // 保证工作目录中没有不可信的半截内容。
                let _ = fs::remove_dir_all(&label_dir);
                Err(error)
            }
        }
    }

    /// 按来源类型分发复制或解压。
    fn materialize_root_into(
        &mut self,
        path: &Path,
        label_dir: &Path,
    ) -> Result<(), MaterializeRootError> {
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            MaterializeRootError::Fatal(
                anyhow::Error::new(error).context(format!("无法读取来源根：{}", path.display())),
            )
        })?;

        if metadata.file_type().is_symlink() && !self.config.follow_symlinks {
            self.warnings
                .push(format!("来源根“{}”是符号链接，已跳过", path.display()));
            return Ok(());
        }
        let followed = if metadata.file_type().is_symlink() {
            fs::metadata(path).unwrap_or(metadata)
        } else {
            metadata
        };

        if followed.is_dir() {
            let mut visited = HashSet::new();
            return self
                .copy_directory_into(path, label_dir, &mut visited)
                .map_err(MaterializeRootError::Fatal);
        }

        if let Some(format) = detect_archive_format(path) {
            if !format.is_supported() {
                // 不可展开的压缩包按原文件保留，来源树标记为未展开节点。
                self.warnings.push(format!(
                    "来源根“{}”的压缩格式（{}）不可展开，按原文件保留",
                    path.display(),
                    format.label()
                ));
                return self
                    .copy_file_into(path, &label_dir.join(file_name_for(path)))
                    .map_err(MaterializeRootError::Fatal);
            }
            let container = ArchiveContainerContext {
                path: path.to_path_buf(),
                display: path.display().to_string(),
                format,
                root_archive: path.to_path_buf(),
            };
            return self.extract_archive(&container, label_dir, Vec::new(), 0);
        }

        self.copy_file_into(path, &label_dir.join(file_name_for(path)))
            .map_err(MaterializeRootError::Fatal)
    }

    /// 解压一个压缩容器到目标目录；密码错误、枚举失败等按该包整体降级处理。
    ///
    /// 参数说明：
    /// - `container`：当前容器上下文（路径、格式、最外层真实压缩包）。
    /// - `dest_dir`：条目落盘目录。
    /// - `chain`：从外层真实压缩包到当前容器的条目链路，用于密码查询。
    /// - `depth`：当前嵌套深度，0 表示最外层。
    fn extract_archive(
        &mut self,
        container: &ArchiveContainerContext,
        dest_dir: &Path,
        chain: Vec<String>,
        depth: usize,
    ) -> Result<(), MaterializeRootError> {
        let Some(adapter) = archive_registry().adapter_for(container.format) else {
            self.warnings.push(format!(
                "压缩包“{}”没有可用解压适配器，已跳过",
                container.display
            ));
            return Ok(());
        };

        let password_key = ArchivePasswordKey::new(&container.root_archive, &chain);
        let password = self.passwords.get(&password_key);
        let session_factory = SessionFactory {
            adapter,
            path: &container.path,
            password,
        };
        // 会话打开失败按性能降级处理：回退逐条目打开，错误交给后续枚举统一归类。
        let source = match session_factory.open() {
            Some(session) => EntrySource::Session(session),
            None => EntrySource::Adapter {
                adapter,
                path: &container.path,
                password,
            },
        };
        self.extract_archive_with_source(
            container,
            dest_dir,
            chain,
            depth,
            source,
            Some(&session_factory),
        )
    }

    /// 使用已建立的条目来源执行解压循环。
    ///
    /// 说明：目录条目在规划阶段直接创建；嵌套条目与普通文件条目彼此独立，在支持
    /// 会话式读取的格式上分别交给 worker 线程池并行解压。
    fn extract_archive_with_source(
        &mut self,
        container: &ArchiveContainerContext,
        dest_dir: &Path,
        chain: Vec<String>,
        depth: usize,
        mut source: EntrySource<'_>,
        session_factory: Option<&SessionFactory<'_>>,
    ) -> Result<(), MaterializeRootError> {
        let entries = match source.list_entries() {
            Ok(entries) => entries,
            Err(error) => {
                if is_retryable_password_error(&error) {
                    // 密码未授权：整个最外层压缩包转为密码占位，解锁后可追加物化。
                    return Err(MaterializeRootError::PasswordPending);
                }
                self.warnings.push(format!(
                    "压缩包“{}”无法枚举条目（{}），已跳过",
                    container.display, error
                ));
                return Ok(());
            }
        };

        let mut written_paths = HashSet::new();
        let mut nested_jobs = Vec::new();
        let mut plain_jobs = Vec::new();
        for entry in entries {
            self.ensure_not_cancelled()?;
            let display_entry = format!("{}!/{}", container.display, entry.path);
            let Some(relative) = sanitize_entry_path(&entry.path) else {
                self.warnings
                    .push(format!("压缩包条目路径不安全，已跳过：{display_entry}"));
                continue;
            };
            // 大小写不敏感文件系统上重名条目会互相覆盖，拒绝后者。
            let conflict_key = relative.to_string_lossy().to_lowercase();
            if !written_paths.insert(conflict_key) {
                self.warnings
                    .push(format!("压缩包条目重名冲突，已跳过：{display_entry}"));
                continue;
            }

            let target = dest_dir.join(&relative);
            if entry.is_dir {
                fs::create_dir_all(&target).map_err(|error| {
                    MaterializeRootError::Fatal(
                        anyhow::Error::new(error)
                            .context(format!("无法创建物化目录：{}", target.display())),
                    )
                })?;
                continue;
            }

            let nested_format = detect_archive_format_by_name(&entry.path)
                .filter(|format| depth < self.config.max_archive_depth && format.is_supported());
            match nested_format {
                Some(ArchiveFormat::Gzip) => {
                    nested_jobs.push(NestedEntryJob::Gzip {
                        entry_path: entry.path,
                        target,
                        display_entry,
                    });
                }
                Some(nested) => {
                    nested_jobs.push(NestedEntryJob::Container {
                        entry_path: entry.path,
                        format: nested,
                        target,
                        display_entry,
                    });
                }
                None => {
                    plain_jobs.push(PlainEntryJob {
                        entry_path: entry.path,
                        target,
                        display_entry,
                    });
                }
            }
        }

        // 普通文件先落盘，嵌套包随后解压并在提升单文件时按序号消歧；
        // 这样同名条目不会互相覆盖，也保持与串行时代"普通文件在前"一致的结果。
        self.extract_plain_entries(plain_jobs, &mut source, session_factory, depth)?;
        self.extract_nested_entries(
            nested_jobs,
            &mut source,
            session_factory,
            container,
            &chain,
            depth,
        )
    }

    /// 解压嵌套条目；支持会话且任务足够多时并行，否则按条目顺序执行。
    fn extract_nested_entries(
        &mut self,
        jobs: Vec<NestedEntryJob>,
        source: &mut EntrySource<'_>,
        session_factory: Option<&SessionFactory<'_>>,
        container: &ArchiveContainerContext,
        chain: &[String],
        depth: usize,
    ) -> Result<(), MaterializeRootError> {
        if jobs.is_empty() {
            return Ok(());
        }

        if let Some(factory) = session_factory
            && depth <= PARALLEL_EXTRACT_MAX_DEPTH
            && jobs.len() >= PARALLEL_EXTRACT_MIN_JOBS
            && let Some(result) =
                self.extract_nested_entries_parallel(&jobs, factory, container, chain, depth)
        {
            return result;
        }

        for job in jobs {
            self.run_nested_entry_job(job, source, container, chain, depth)?;
        }
        Ok(())
    }

    /// 执行单个嵌套条目任务；顺序路径与并行 worker 共用同一实现。
    fn run_nested_entry_job(
        &mut self,
        job: NestedEntryJob,
        source: &mut EntrySource<'_>,
        container: &ArchiveContainerContext,
        chain: &[String],
        depth: usize,
    ) -> Result<(), MaterializeRootError> {
        match job {
            NestedEntryJob::Gzip {
                entry_path,
                target,
                display_entry,
            } => self.extract_nested_gzip(source, &entry_path, &target, &display_entry),
            NestedEntryJob::Container {
                entry_path,
                format,
                target,
                display_entry,
            } => self.extract_nested_container(
                source,
                container,
                &entry_path,
                format,
                &target,
                chain.to_vec(),
                depth,
                &display_entry,
            ),
        }
    }

    /// 用固定大小 worker 线程池并行解压嵌套条目。
    ///
    /// 说明：每个 worker 持有独立的容器会话与轻量物化器状态（嵌套解压不涉及顶层
    /// roots/used_labels/password_pending），完成后统一合并文件计数与警告。
    ///
    /// 返回值：`None` 表示无法为全部 worker 建立独立会话，调用方退回顺序解压；
    /// `Some` 表示并行解压已执行，成败由内部结果给出。
    fn extract_nested_entries_parallel(
        &mut self,
        jobs: &[NestedEntryJob],
        factory: &SessionFactory<'_>,
        container: &ArchiveContainerContext,
        chain: &[String],
        depth: usize,
    ) -> Option<Result<(), MaterializeRootError>> {
        let worker_count = parallel_worker_count(jobs.len());
        let mut sessions = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            sessions.push(factory.open()?);
        }

        let config = self.config;
        let passwords = self.passwords;
        let cancellation = self.cancellation.clone();
        let root = self.root.clone();
        let next_index = AtomicUsize::new(0);
        let failure: Mutex<Option<MaterializeRootError>> = Mutex::new(None);
        let outcomes: Mutex<Vec<NestedWorkerOutcome>> = Mutex::new(Vec::new());
        let base_files = self.files_written;

        std::thread::scope(|scope| {
            for session in sessions {
                let next_index = &next_index;
                let failure = &failure;
                let outcomes = &outcomes;
                let cancellation = cancellation.clone();
                let root = root.clone();
                scope.spawn(move || {
                    // 嵌套 worker 不上报进度：各自持有局部计数会让全局进度抖动，
                    // 外层阶段会在合并计数后继续上报。
                    let mut worker = WorkspaceMaterializer {
                        config,
                        passwords,
                        cancellation: &cancellation,
                        progress: None,
                        root,
                        used_labels: HashSet::new(),
                        files_written: 0,
                        roots: Vec::new(),
                        password_pending: Vec::new(),
                        warnings: Vec::new(),
                    };
                    let mut source = EntrySource::Session(session);
                    (|| {
                        loop {
                            if failure.lock().map(|slot| slot.is_some()).unwrap_or(false) {
                                return;
                            }
                            if cancellation.is_cancelled() {
                                store_parallel_failure(failure, MaterializeRootError::Cancelled);
                                return;
                            }
                            let index = next_index.fetch_add(1, Ordering::Relaxed);
                            let Some(job) = jobs.get(index) else {
                                return;
                            };
                            if let Err(error) = worker.run_nested_entry_job(
                                job.clone(),
                                &mut source,
                                container,
                                chain,
                                depth,
                            ) {
                                store_parallel_failure(failure, error);
                                return;
                            }
                        }
                    })();

                    if let Ok(mut guard) = outcomes.lock() {
                        guard.push(NestedWorkerOutcome {
                            files_written: worker.files_written,
                            warnings: worker.warnings,
                        });
                    }
                });
            }
        });

        let error = failure.into_inner().ok().flatten();
        let outcomes = outcomes.into_inner().unwrap_or_default();
        self.files_written = base_files
            + outcomes
                .iter()
                .map(|outcome| outcome.files_written)
                .sum::<usize>();
        for outcome in outcomes {
            self.warnings.extend(outcome.warnings);
        }
        Some(match error {
            Some(error) => Err(error),
            None => Ok(()),
        })
    }

    /// 解压普通文件条目；支持会话且任务足够多时并行，否则按条目顺序执行。
    fn extract_plain_entries(
        &mut self,
        jobs: Vec<PlainEntryJob>,
        source: &mut EntrySource<'_>,
        session_factory: Option<&SessionFactory<'_>>,
        depth: usize,
    ) -> Result<(), MaterializeRootError> {
        if jobs.is_empty() {
            return Ok(());
        }

        if let Some(factory) = session_factory
            && depth <= PARALLEL_EXTRACT_MAX_DEPTH
            && jobs.len() >= PARALLEL_EXTRACT_MIN_JOBS
            && let Some(result) = self.extract_plain_entries_parallel(&jobs, factory)
        {
            return result;
        }

        for job in jobs {
            stream_entry_to_path(source, &job.entry_path, &job.target, &job.display_entry)?;
            self.files_written += 1;
            if self.files_written.is_multiple_of(PROGRESS_REPORT_INTERVAL) {
                self.report_progress(&job.display_entry);
            }
        }
        Ok(())
    }

    /// 用固定大小 worker 线程池并行解压普通文件条目。
    ///
    /// 返回值：`None` 表示无法为全部 worker 建立独立会话，调用方退回顺序解压；
    /// `Some` 表示并行解压已执行，成败由内部结果给出。
    fn extract_plain_entries_parallel(
        &mut self,
        jobs: &[PlainEntryJob],
        factory: &SessionFactory<'_>,
    ) -> Option<Result<(), MaterializeRootError>> {
        let worker_count = parallel_worker_count(jobs.len());
        let mut sessions = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            sessions.push(factory.open()?);
        }

        let cancellation = self.cancellation.clone();
        let progress = self.progress.cloned();
        let next_index = AtomicUsize::new(0);
        let completed = AtomicUsize::new(0);
        let failure: Mutex<Option<MaterializeRootError>> = Mutex::new(None);
        let base_files = self.files_written;

        std::thread::scope(|scope| {
            for session in sessions {
                let next_index = &next_index;
                let completed = &completed;
                let failure = &failure;
                let cancellation = cancellation.clone();
                let progress = progress.clone();
                scope.spawn(move || {
                    let mut source = EntrySource::Session(session);
                    loop {
                        if failure.lock().map(|slot| slot.is_some()).unwrap_or(false) {
                            return;
                        }
                        if cancellation.is_cancelled() {
                            store_parallel_failure(failure, MaterializeRootError::Cancelled);
                            return;
                        }
                        let index = next_index.fetch_add(1, Ordering::Relaxed);
                        let Some(job) = jobs.get(index) else {
                            return;
                        };
                        if let Err(error) = stream_entry_to_path(
                            &mut source,
                            &job.entry_path,
                            &job.target,
                            &job.display_entry,
                        ) {
                            store_parallel_failure(failure, error);
                            return;
                        }
                        let finished = completed.fetch_add(1, Ordering::Relaxed) + 1;
                        if finished.is_multiple_of(PROGRESS_REPORT_INTERVAL)
                            && let Some(sender) = progress.as_ref()
                        {
                            let _ = sender.send(SourceTreeScanProgress {
                                phase: SourceLoadPhase::Materializing,
                                scanned: base_files + finished,
                                current: job.display_entry.clone(),
                            });
                        }
                    }
                });
            }
        });

        let error = failure.into_inner().ok().flatten();
        self.files_written = base_files + completed.load(Ordering::Relaxed);
        Some(match error {
            Some(error) => Err(error),
            None => Ok(()),
        })
    }

    /// 解压嵌套单文件 gzip 条目为普通文件（落盘名沿用条目名去掉 `.gz` 的约定）。
    fn extract_nested_gzip(
        &mut self,
        source: &mut EntrySource<'_>,
        entry_path: &str,
        target: &Path,
        display_entry: &str,
    ) -> Result<(), MaterializeRootError> {
        let file_stem = target
            .file_name()
            .and_then(|name| Path::new(name).file_stem().map(|stem| stem.to_os_string()));
        let Some(file_stem) = file_stem else {
            self.warnings.push(format!(
                "嵌套 gzip 条目名无法推导落盘名，已跳过：{display_entry}"
            ));
            return Ok(());
        };
        let target = target
            .parent()
            .map(|parent| parent.join(&file_stem))
            .unwrap_or_else(|| PathBuf::from(&file_stem));
        self.stream_entry_to_file(source, entry_path, &target, display_entry)
    }

    /// 解压嵌套容器条目：先把容器条目流式落盘到 scratch，再从 scratch 递归解压为同名目录。
    ///
    /// 说明：嵌套容器可能远大于内存，统一经临时文件解压，避免把整包读入内存。
    fn extract_nested_container(
        &mut self,
        source: &mut EntrySource<'_>,
        container: &ArchiveContainerContext,
        entry_path: &str,
        nested_format: ArchiveFormat,
        target: &Path,
        chain: Vec<String>,
        depth: usize,
        display_entry: &str,
    ) -> Result<(), MaterializeRootError> {
        let nested_dir = target
            .parent()
            .map(|parent| parent.join(nested_dir_name_for(entry_path)))
            .unwrap_or_else(|| PathBuf::from(nested_dir_name_for(entry_path)));
        fs::create_dir_all(&nested_dir).map_err(|error| {
            MaterializeRootError::Fatal(
                anyhow::Error::new(error)
                    .context(format!("无法创建嵌套物化目录：{}", nested_dir.display())),
            )
        })?;

        let scratch_dir = self.root.join(".argus-scratch");
        fs::create_dir_all(&scratch_dir).map_err(|error| {
            MaterializeRootError::Fatal(anyhow::Error::new(error).context(format!(
                "无法创建嵌套解压暂存目录：{}",
                scratch_dir.display()
            )))
        })?;
        let scratch_file = next_scratch_file(&self.root, depth);
        if let Err(error) = stream_entry_to_path(source, entry_path, &scratch_file, display_entry) {
            let _ = fs::remove_file(&scratch_file);
            return Err(error);
        }

        let mut nested_chain = chain;
        nested_chain.push(entry_path.to_string());
        let nested_container = ArchiveContainerContext {
            path: scratch_file.clone(),
            display: display_entry.to_string(),
            format: nested_format,
            root_archive: container.root_archive.clone(),
        };
        let result = self.extract_archive(&nested_container, &nested_dir, nested_chain, depth + 1);
        let _ = fs::remove_file(&scratch_file);
        // 嵌套包只解出一个普通文件时提升到父级，去掉以压缩包名命名的包装目录。
        if result.is_ok() {
            Self::promote_single_file_out_of_dir(&nested_dir);
        }
        result
    }

    /// 把包装目录中唯一的一个普通文件提升到父级；目标名与包装目录同名时经暂存名中转，
    /// 重名按扩展名前序号消歧。任何失败都保持原布局，不阻断加载。
    fn promote_single_file_out_of_dir(wrapper_dir: &Path) {
        let Ok(entries) = fs::read_dir(wrapper_dir) else {
            return;
        };
        let mut single_file: Option<PathBuf> = None;
        let mut entry_count = 0_usize;
        for entry in entries.flatten() {
            entry_count += 1;
            if entry_count > 1 {
                break;
            }
            let path = entry.path();
            single_file = path.is_file().then_some(path);
        }
        if entry_count != 1 {
            return;
        }
        let Some(file_path) = single_file else {
            return;
        };
        let Some(parent) = wrapper_dir.parent() else {
            return;
        };
        let Some(file_name) = file_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            return;
        };
        let (stem, extension) = Self::split_file_name_extension(&file_name);
        let mut candidate = file_name.clone();
        let mut suffix = 1_usize;
        // 并行解压时多个嵌套包会同时向同一父目录提升文件；加锁保证重名消歧与落位
        // 不互相竞争，避免 rename 覆盖同名文件。
        let _promote_guard = NESTED_PROMOTE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // 包装目录自身占用的名称即将释放，视为可用。
        while parent.join(&candidate).exists() && parent.join(&candidate) != wrapper_dir {
            suffix += 1;
            candidate = format!("{stem} ({suffix}){extension}");
        }
        let target = parent.join(&candidate);
        if target == wrapper_dir {
            // 目标名正被包装目录占用：先经暂存名移出，删除目录后落位。
            let staging = parent.join(format!(".argus-promote-{file_name}"));
            if fs::rename(&file_path, &staging).is_err()
                || fs::remove_dir(wrapper_dir).is_err()
                || fs::rename(&staging, &target).is_err()
            {
                let _ = fs::rename(&staging, &file_path);
            }
        } else if fs::rename(&file_path, &target).is_ok() {
            let _ = fs::remove_dir(wrapper_dir);
        }
    }

    /// 把文件名拆为词干与含点的扩展名；无扩展名时扩展名为空。
    fn split_file_name_extension(file_name: &str) -> (&str, &str) {
        match file_name.rfind('.') {
            Some(position) if position > 0 => file_name.split_at(position),
            _ => (file_name, ""),
        }
    }

    /// 流式解压单个条目到文件，并计入物化文件数与进度。
    fn stream_entry_to_file(
        &mut self,
        source: &mut EntrySource<'_>,
        entry_path: &str,
        target: &Path,
        display_entry: &str,
    ) -> Result<(), MaterializeRootError> {
        stream_entry_to_path(source, entry_path, target, display_entry)?;
        self.files_written += 1;
        if self.files_written.is_multiple_of(PROGRESS_REPORT_INTERVAL) {
            self.report_progress(display_entry);
        }
        Ok(())
    }

    /// 流式复制普通文件。
    fn copy_file_into(&mut self, source: &Path, target: &Path) -> Result<()> {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("无法创建工作目录：{}", parent.display()))?;
        }
        let mut reader = File::open(source)
            .with_context(|| format!("无法读取来源文件：{}", source.display()))?;
        let mut writer = File::create(target)
            .with_context(|| format!("无法创建工作目录文件：{}", target.display()))?;
        let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
        loop {
            self.ensure_not_cancelled()?;
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            writer.write_all(&buffer[..read])?;
        }
        self.files_written += 1;
        if self.files_written.is_multiple_of(PROGRESS_REPORT_INTERVAL) {
            self.report_progress(&source.display().to_string());
        }
        Ok(())
    }

    /// 递归复制目录；跟随符号链接时用规范路径集合防循环。
    fn copy_directory_into(
        &mut self,
        source: &Path,
        target: &Path,
        visited: &mut HashSet<PathBuf>,
    ) -> Result<()> {
        self.ensure_not_cancelled()?;
        let canonical = source
            .canonicalize()
            .unwrap_or_else(|_| source.to_path_buf());
        if !visited.insert(canonical) {
            self.warnings.push(format!(
                "目录“{}”形成符号链接循环，已跳过",
                source.display()
            ));
            return Ok(());
        }
        fs::create_dir_all(target)
            .with_context(|| format!("无法创建工作目录：{}", target.display()))?;

        let entries =
            fs::read_dir(source).with_context(|| format!("无法读取目录：{}", source.display()))?;
        for entry in entries {
            self.ensure_not_cancelled()?;
            let entry = entry?;
            let entry_path = entry.path();
            let entry_target = target.join(entry.file_name());
            let metadata = fs::symlink_metadata(&entry_path)?;
            if metadata.file_type().is_symlink() && !self.config.follow_symlinks {
                self.warnings
                    .push(format!("已跳过符号链接：{}", entry_path.display()));
                continue;
            }
            let followed = if metadata.file_type().is_symlink() {
                fs::metadata(&entry_path).unwrap_or(metadata)
            } else {
                metadata
            };
            if followed.is_dir() {
                self.copy_directory_into(&entry_path, &entry_target, visited)?;
            } else if followed.is_file() {
                self.materialize_directory_file(&entry_path, &entry_target)?;
            }
        }
        Ok(())
    }

    /// 物化目录中的单个文件：压缩包同步展开为去扩展名的同名目录，其余按原文件复制。
    ///
    /// 说明：目录内加密压缩包不提供占位流程，未展开的包按原文件保留（来源树标记未展开）。
    fn materialize_directory_file(&mut self, source: &Path, target: &Path) -> Result<()> {
        let Some(format) = detect_archive_format(source).filter(|format| format.is_supported())
        else {
            return self.copy_file_into(source, target);
        };

        let stem_dir = target.with_file_name(nested_dir_name_for(&file_name_for(source)));
        let container = ArchiveContainerContext {
            path: source.to_path_buf(),
            display: source.display().to_string(),
            format,
            root_archive: source.to_path_buf(),
        };
        match self.extract_archive(&container, &stem_dir, Vec::new(), 0) {
            Ok(()) => Ok(()),
            Err(MaterializeRootError::PasswordPending) => {
                let _ = fs::remove_dir_all(&stem_dir);
                self.warnings.push(format!(
                    "目录中的加密压缩包未展开，按原文件保留：{}",
                    source.display()
                ));
                self.copy_file_into(source, target)
            }
            Err(MaterializeRootError::Cancelled) => Err(anyhow::anyhow!("已取消日志加载")),
            Err(MaterializeRootError::Fatal(error)) => Err(error),
        }
    }

    /// 解压产物若恰好只有一个普通文件，把它提升为工作目录顶层的文件根。
    ///
    /// 提升后标签目录删除、标签名从占用集合释放；重名时在扩展名前追加序号。
    /// 任何异常都退回原标签目录布局，不阻断加载。
    fn promote_single_file_root(&mut self, label: &str, label_dir: &Path) -> PathBuf {
        let fallback = |materializer: &mut Self| {
            materializer.used_labels.insert(label.to_lowercase());
            label_dir.to_path_buf()
        };
        let entries = match fs::read_dir(label_dir) {
            Ok(entries) => entries,
            Err(_) => return fallback(self),
        };
        let mut single_file: Option<PathBuf> = None;
        let mut entry_count = 0_usize;
        for entry in entries.flatten() {
            entry_count += 1;
            if entry_count > 1 {
                break;
            }
            let path = entry.path();
            single_file = path.is_file().then_some(path);
        }
        if entry_count != 1 {
            return fallback(self);
        }
        let Some(file_path) = single_file else {
            return fallback(self);
        };
        let Some(file_name) = file_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            return fallback(self);
        };
        // 先释放标签占用：目标文件名与标签同名时（如 gzip 派生的 access.log）才会被放行。
        let label_key = label.to_lowercase();
        self.used_labels.remove(&label_key);
        let unique_name = self.unique_top_file_name(&file_name, label_dir);
        let target = self.root.join(&unique_name);
        if target == label_dir {
            // 目标名正被标签目录占用：先经临时名移出文件，删除目录后落位。
            let staging = self.root.join(format!(".argus-promote-{file_name}"));
            if fs::rename(&file_path, &staging).is_err()
                || fs::remove_dir(label_dir).is_err()
                || fs::rename(&staging, &target).is_err()
            {
                let _ = fs::rename(&staging, &file_path);
                return fallback(self);
            }
        } else if fs::rename(&file_path, &target).is_err() {
            return fallback(self);
        } else {
            let _ = fs::remove_dir(label_dir);
        }
        target
    }

    /// 生成工作目录顶层不冲突的文件名；序号插在扩展名之前（`app.log` → `app (2).log`）。
    ///
    /// `removing_dir` 是即将删除的标签目录，其占用的名称视为可用。
    fn unique_top_file_name(&mut self, file_name: &str, removing_dir: &Path) -> String {
        let (stem, extension) = Self::split_file_name_extension(file_name);
        let mut candidate = file_name.to_string();
        let mut suffix = 1_usize;
        while {
            let candidate_path = self.root.join(&candidate);
            (candidate_path.exists() && candidate_path != removing_dir)
                || !self.used_labels.insert(candidate.to_lowercase())
        } {
            suffix += 1;
            candidate = format!("{stem} ({suffix}){extension}");
        }
        candidate
    }

    /// 生成不冲突的顶层目录名。
    fn unique_top_label(&mut self, base: &str) -> String {
        let sanitized = sanitize_label(base);
        let mut candidate = sanitized.clone();
        let mut suffix = 1_usize;
        while !self.used_labels.insert(candidate.to_lowercase()) {
            suffix += 1;
            candidate = format!("{sanitized} ({suffix})");
        }
        candidate
    }

    /// 检查取消令牌；取消时整个物化立即终止。
    fn ensure_not_cancelled(&self) -> Result<(), MaterializeRootError> {
        if self.cancellation.is_cancelled() {
            return Err(MaterializeRootError::Cancelled);
        }
        Ok(())
    }

    /// 上报当前物化进度。
    fn report_progress(&self, current: &str) {
        if let Some(sender) = self.progress {
            let _ = sender.send(SourceTreeScanProgress {
                phase: SourceLoadPhase::Materializing,
                scanned: self.files_written,
                current: current.to_string(),
            });
        }
    }
}

/// 返回来源根用于顶层目录的基础标签：目录取目录名，普通文件取文件名主干，
/// 压缩包取去掉全部压缩扩展名的名称。
fn display_label_for_path(path: &Path) -> String {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "source".to_string());
    if path.is_dir() {
        return file_name;
    }
    if detect_archive_format(path).is_some_and(|format| format.is_supported()) {
        return nested_dir_name_for(&file_name);
    }
    Path::new(&file_name)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or(file_name)
}

/// 返回去掉全部可识别压缩扩展名的名称；`inner.tar.gz` → `inner`，`x.log.gz` → `x.log`。
fn nested_dir_name_for(file_name: &str) -> String {
    let mut current = file_name.to_string();
    while let Some(format) = detect_archive_format_by_name(&current) {
        if !format.is_supported() {
            break;
        }
        let stem = Path::new(&current)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        if stem.is_empty() || stem == current {
            break;
        }
        current = stem;
    }
    if current.is_empty() {
        "source".to_string()
    } else {
        current
    }
}

/// 返回普通文件来源的落盘文件名。
fn file_name_for(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "source.log".to_string())
}

/// 返回进度展示用的来源路径文本。
fn display_path_for_progress(path: &Path) -> String {
    path.display().to_string()
}

/// 清洗顶层目录名：保留字母数字、中文和 `-_. ()`，其余替换为 `_`。
fn sanitize_label(label: &str) -> String {
    let sanitized = label
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, '-' | '_' | '.' | ' ' | '(' | ')')
            {
                character
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim()
        .trim_matches('.')
        .to_string();
    if sanitized.is_empty() {
        "source".to_string()
    } else {
        sanitized
    }
}

/// 把压缩包条目路径清洗为安全的相对路径；含 `..`、控制字符、Windows 盘符/保留名的
/// 条目返回 `None` 拒绝落盘（zip slip 防护）。绝对路径因按 `/` 拆分自然落入工作目录内。
fn sanitize_entry_path(raw: &str) -> Option<PathBuf> {
    let normalized = raw.replace('\\', "/");
    let mut output = PathBuf::new();
    for component in normalized.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." || component.contains(':') || component.chars().any(char::is_control) {
            return None;
        }
        let trimmed = component.trim_end_matches(['.', ' ']);
        if trimmed.is_empty() {
            return None;
        }
        if is_windows_reserved_name(trimmed) {
            return None;
        }
        output.push(trimmed);
    }
    if output.as_os_str().is_empty() {
        return None;
    }
    Some(output)
}

/// 判断名称是否为 Windows 保留设备名；跨平台打包 Windows 时同名文件会创建失败或被劫持。
fn is_windows_reserved_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_uppercase();
    matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::paths::isolated_test_dir;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    /// 构造默认取消令牌，物化测试无需真实取消场景。
    fn test_cancellation() -> CancellationToken {
        CancellationToken::new()
    }

    /// 在独立测试目录中创建一个普通文本文件并返回路径。
    fn write_test_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("应创建测试文件父目录");
        }
        fs::write(&path, content).expect("应写入测试文件");
        path
    }

    /// 在独立测试目录中创建一个 zip 包并写入给定条目。
    fn write_test_zip(path: &Path, entries: &[(&str, &[u8])]) -> PathBuf {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("应创建测试 zip 父目录");
        }
        let file = File::create(path).expect("应创建测试 zip");
        let mut writer = ZipWriter::new(file);
        for (name, content) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .expect("应写入 zip 条目");
            writer.write_all(content).expect("应写入 zip 条目内容");
        }
        writer.finish().expect("应完成测试 zip");
        path.to_path_buf()
    }

    /// 验证条目路径清洗拒绝 zip slip、盘符、控制字符和 Windows 保留名。
    #[test]
    fn sanitize_entry_path_rejects_unsafe_components() {
        assert_eq!(
            sanitize_entry_path("dir/sub/app.log"),
            Some(PathBuf::from("dir/sub/app.log"))
        );
        // 绝对路径按 `/` 拆分后自然落入工作目录内，不产生逃逸。
        assert_eq!(
            sanitize_entry_path("/etc/passwd"),
            Some(PathBuf::from("etc/passwd"))
        );
        assert_eq!(sanitize_entry_path("../escape.log"), None);
        assert_eq!(sanitize_entry_path("dir/../../escape.log"), None);
        assert_eq!(sanitize_entry_path("C:/windows/system.log"), None);
        assert_eq!(sanitize_entry_path("bad\u{1}.log"), None);
        assert_eq!(sanitize_entry_path("CON.log"), None);
        assert_eq!(sanitize_entry_path("dir/./../x.log"), None);
    }

    /// 验证顶层标签清洗和压缩扩展名逐级剥离。
    #[test]
    fn label_sanitization_strips_archive_extensions() {
        assert_eq!(nested_dir_name_for("inner.tar.gz"), "inner");
        assert_eq!(nested_dir_name_for("x.log.gz"), "x.log");
        assert_eq!(nested_dir_name_for("backup.zip"), "backup");
        assert_eq!(sanitize_label("my logs/2026"), "my logs_2026");
        assert_eq!(sanitize_label("..."), "source");
    }

    /// 验证普通文件复制为 `<标签>/<文件名>` 且内容一致。
    #[test]
    fn materialize_plain_file_copies_content() {
        let dir = isolated_test_dir("workspace-plain");
        let source = write_test_file(&dir, "app.log", "line1\nline2\n");

        let workspace = materialize_sources(
            &[source],
            &LoaderConfig::default(),
            &ArchivePasswordStore::default(),
            &test_cancellation(),
            None,
        )
        .expect("普通文件物化应成功");

        let materialized = workspace.root.join("app/app.log");
        assert_eq!(
            fs::read_to_string(&materialized).expect("应读取物化文件"),
            "line1\nline2\n"
        );
        assert_eq!(workspace.materialized_files, 1);
    }

    /// 验证目录递归复制保留结构并跳过符号链接。
    #[test]
    fn materialize_directory_copies_tree_and_skips_symlinks() {
        let dir = isolated_test_dir("workspace-dir");
        let source_dir = dir.join("logs");
        write_test_file(&source_dir, "a.log", "a");
        write_test_file(&source_dir, "sub/b.log", "b");
        #[cfg(unix)]
        std::os::unix::fs::symlink("a.log", source_dir.join("link.log"))
            .expect("应创建测试符号链接");

        let workspace = materialize_sources(
            &[source_dir],
            &LoaderConfig::default(),
            &ArchivePasswordStore::default(),
            &test_cancellation(),
            None,
        )
        .expect("目录物化应成功");

        assert!(workspace.root.join("logs/a.log").exists());
        assert!(workspace.root.join("logs/sub/b.log").exists());
        #[cfg(unix)]
        assert!(!workspace.root.join("logs/link.log").exists());
    }

    /// 验证 zip 物化解压保留目录结构，并拒绝 zip slip 和重名冲突条目。
    #[test]
    fn materialize_zip_extracts_entries_and_rejects_unsafe() {
        let dir = isolated_test_dir("workspace-zip");
        let archive = write_test_zip(
            &dir.join("bundle.zip"),
            &[
                ("app/application.log", b"app" as &[u8]),
                ("../evil.log", b"evil"),
                ("app/duplicate.log", b"first"),
                ("app/DUPLICATE.log", b"second"),
            ],
        );

        let workspace = materialize_sources(
            &[archive],
            &LoaderConfig::default(),
            &ArchivePasswordStore::default(),
            &test_cancellation(),
            None,
        )
        .expect("zip 物化应成功");

        let root = workspace.root.join("bundle");
        assert_eq!(
            fs::read(root.join("app/application.log")).expect("应读取解压条目"),
            b"app"
        );
        assert!(!workspace.root.join("evil.log").exists());
        assert!(!root.join("../evil.log").exists());
        // 大小写冲突条目被跳过后，文件中保留的是先写入的内容（未被后者覆盖）。
        assert_eq!(
            fs::read(root.join("app/duplicate.log")).expect("应读取冲突条目"),
            b"first"
        );
        assert!(
            workspace
                .warnings
                .iter()
                .any(|warning| warning.contains("不安全") || warning.contains("重名冲突"))
        );
    }

    /// 验证嵌套 zip 被解压为去扩展名的同名目录。
    #[test]
    fn materialize_zip_extracts_nested_archive_into_stem_directory() {
        let dir = isolated_test_dir("workspace-nested");
        // 先造一个内存中的内层 zip。
        let mut inner_cursor = std::io::Cursor::new(Vec::new());
        {
            let mut inner_writer = ZipWriter::new(&mut inner_cursor);
            inner_writer
                .start_file("inner.log", SimpleFileOptions::default())
                .expect("应写入内层条目");
            inner_writer
                .write_all(b"inner")
                .expect("应写入内层条目内容");
            inner_writer.finish().expect("应完成内层 zip");
        }
        let inner_bytes = inner_cursor.into_inner();
        let archive = write_test_zip(
            &dir.join("outer.zip"),
            &[("nested/inner.zip", inner_bytes.as_slice())],
        );

        let workspace = materialize_sources(
            &[archive],
            &LoaderConfig::default(),
            &ArchivePasswordStore::default(),
            &test_cancellation(),
            None,
        )
        .expect("嵌套 zip 物化应成功");

        assert_eq!(
            fs::read(workspace.root.join("outer/nested/inner.log")).expect("应读取嵌套解压条目"),
            b"inner"
        );
        assert!(
            !workspace.root.join("outer/nested/inner").exists(),
            "单文件嵌套包的包装目录应被移除"
        );
    }

    /// 验证嵌套单文件包与父目录既有文件同名时按序号消歧。
    #[test]
    fn nested_single_file_archive_collision_appends_suffix() {
        let dir = isolated_test_dir("workspace-nested-collision");
        let mut inner_cursor = std::io::Cursor::new(Vec::new());
        {
            let mut inner_writer = ZipWriter::new(&mut inner_cursor);
            inner_writer
                .start_file("app.log", SimpleFileOptions::default())
                .expect("应写入内层条目");
            inner_writer.write_all(b"nested").expect("应写入内层内容");
            inner_writer.finish().expect("应完成内层 zip");
        }
        let inner_bytes = inner_cursor.into_inner();
        let archive = write_test_zip(
            &dir.join("outer.zip"),
            &[
                ("monitorThread/app.log", b"plain".as_slice()),
                ("monitorThread/inner.zip", inner_bytes.as_slice()),
            ],
        );

        let workspace = materialize_sources(
            &[archive],
            &LoaderConfig::default(),
            &ArchivePasswordStore::default(),
            &test_cancellation(),
            None,
        )
        .expect("嵌套同名冲突物化应成功");

        // 兄弟文件先落盘，嵌套包同名文件在扩展名前追加序号。
        assert_eq!(
            fs::read(workspace.root.join("outer/monitorThread/app.log")).expect("应读取既有文件"),
            b"plain"
        );
        assert_eq!(
            fs::read(workspace.root.join("outer/monitorThread/app (2).log"))
                .expect("应读取提升后的嵌套文件"),
            b"nested"
        );
        assert!(
            !workspace.root.join("outer/monitorThread/inner").exists(),
            "包装目录应被移除"
        );
    }

    /// 验证嵌套包内含目录时保留包装目录布局。
    #[test]
    fn nested_archive_with_directory_keeps_wrapper() {
        let dir = isolated_test_dir("workspace-nested-dir");
        let mut inner_cursor = std::io::Cursor::new(Vec::new());
        {
            let mut inner_writer = ZipWriter::new(&mut inner_cursor);
            inner_writer
                .start_file("logs/a.log", SimpleFileOptions::default())
                .expect("应写入内层条目");
            inner_writer.write_all(b"a").expect("应写入内层内容");
            inner_writer.finish().expect("应完成内层 zip");
        }
        let inner_bytes = inner_cursor.into_inner();
        let archive = write_test_zip(
            &dir.join("outer.zip"),
            &[("monitorThread/inner.zip", inner_bytes.as_slice())],
        );

        let workspace = materialize_sources(
            &[archive],
            &LoaderConfig::default(),
            &ArchivePasswordStore::default(),
            &test_cancellation(),
            None,
        )
        .expect("含目录嵌套包物化应成功");

        assert!(
            workspace
                .root
                .join("outer/monitorThread/inner/logs/a.log")
                .is_file()
        );
        assert!(workspace.root.join("outer/monitorThread/inner").is_dir());
    }

    /// 构造一个只含单个指定名称文件的内层 zip 字节。
    fn nested_zip_bytes(file_name: &str, payload: &str) -> Vec<u8> {
        let mut inner_cursor = std::io::Cursor::new(Vec::new());
        {
            let mut inner_writer = ZipWriter::new(&mut inner_cursor);
            inner_writer
                .start_file(file_name, SimpleFileOptions::default())
                .expect("应写入内层条目");
            inner_writer
                .write_all(payload.as_bytes())
                .expect("应写入内层内容");
            inner_writer.finish().expect("应完成内层 zip");
        }
        inner_cursor.into_inner()
    }

    /// 验证超过并行阈值的多个嵌套包全部解压并提升单文件。
    #[test]
    fn materialize_parallel_nested_archives_promote_all_files() {
        let dir = isolated_test_dir("workspace-nested-parallel");
        let blobs = (0..12)
            .map(|index| {
                nested_zip_bytes(&format!("log-{index:02}.log"), &format!("payload-{index}"))
            })
            .collect::<Vec<_>>();
        let names = (0..12)
            .map(|index| format!("nested/pack-{index:02}.zip"))
            .collect::<Vec<_>>();
        let entries = names
            .iter()
            .zip(blobs.iter())
            .map(|(name, blob)| (name.as_str(), blob.as_slice()))
            .collect::<Vec<_>>();
        let archive = write_test_zip(&dir.join("outer.zip"), &entries);

        let workspace = materialize_sources(
            &[archive],
            &LoaderConfig::default(),
            &ArchivePasswordStore::default(),
            &test_cancellation(),
            None,
        )
        .expect("多嵌套包并行物化应成功");

        for index in 0..12 {
            let path = workspace
                .root
                .join(format!("outer/nested/log-{index:02}.log"));
            assert_eq!(
                fs::read_to_string(&path).expect("应读取提升后的嵌套文件"),
                format!("payload-{index}")
            );
        }
        // 嵌套包本身不计入文件数，只统计实际解出的文件。
        assert_eq!(workspace.materialized_files, 12);
        assert!(
            workspace.warnings.is_empty(),
            "并行嵌套解压不应产生警告：{:?}",
            workspace.warnings
        );
    }

    /// 验证并行提升同名文件时按序号消歧且不丢失任何文件。
    #[test]
    fn materialize_parallel_nested_promotion_keeps_all_same_name_files() {
        let dir = isolated_test_dir("workspace-nested-parallel-same-name");
        let blobs = (0..12)
            .map(|index| nested_zip_bytes("app.log", &format!("payload-{index}")))
            .collect::<Vec<_>>();
        let names = (0..12)
            .map(|index| format!("nested/pack-{index:02}.zip"))
            .collect::<Vec<_>>();
        let entries = names
            .iter()
            .zip(blobs.iter())
            .map(|(name, blob)| (name.as_str(), blob.as_slice()))
            .collect::<Vec<_>>();
        let archive = write_test_zip(&dir.join("outer.zip"), &entries);

        let workspace = materialize_sources(
            &[archive],
            &LoaderConfig::default(),
            &ArchivePasswordStore::default(),
            &test_cancellation(),
            None,
        )
        .expect("同名嵌套文件并行物化应成功");

        let mut payloads = fs::read_dir(workspace.root.join("outer/nested"))
            .expect("应读取嵌套目录")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_file())
            .map(|entry| fs::read_to_string(entry.path()).expect("应读取提升后的文件"))
            .collect::<Vec<_>>();
        payloads.sort();
        let mut expected = (0..12)
            .map(|index| format!("payload-{index}"))
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(payloads, expected, "并行提升不得覆盖或丢失同名文件");
    }

    /// 验证顶层 gzip 单文件包解压为去 `.gz` 的普通文件并直接提升为顶层文件根。
    #[test]
    fn materialize_gzip_extracts_single_file() {
        let dir = isolated_test_dir("workspace-gzip");
        fs::create_dir_all(&dir).expect("应创建测试目录");
        let gzip_path = dir.join("access.log.gz");
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"gz-line").expect("应写入 gzip 内容");
        let bytes = encoder.finish().expect("应完成 gzip 编码");
        fs::write(&gzip_path, bytes).expect("应写入测试 gzip");

        let workspace = materialize_sources(
            &[gzip_path],
            &LoaderConfig::default(),
            &ArchivePasswordStore::default(),
            &test_cancellation(),
            None,
        )
        .expect("gzip 物化应成功");

        assert_eq!(
            fs::read(workspace.root.join("access.log")).expect("应读取 gzip 解压文件"),
            b"gz-line"
        );
        assert!(!workspace.root.join("access.log").is_dir());
        assert_eq!(workspace.roots.len(), 1);
        assert_eq!(workspace.roots[0].path, workspace.root.join("access.log"));
    }

    /// 验证解压不受总量或条目数限制：大条目与大量条目都必须完整展开。
    #[test]
    fn materialize_extracts_without_total_size_or_entry_count_limits() {
        let dir = isolated_test_dir("workspace-unlimited");
        let large_entry = vec![b'x'; 2 * 1024 * 1024];
        let small_entry = vec![b'y'; 4096];
        let small_names = (0..128)
            .map(|index| format!("many/entry-{index:03}.log"))
            .collect::<Vec<_>>();
        let mut entries: Vec<(&str, &[u8])> = vec![("large.log", large_entry.as_slice())];
        for name in &small_names {
            entries.push((name.as_str(), small_entry.as_slice()));
        }
        let archive = write_test_zip(&dir.join("big.zip"), &entries);

        let workspace = materialize_sources(
            &[archive],
            &LoaderConfig::default(),
            &ArchivePasswordStore::default(),
            &test_cancellation(),
            None,
        )
        .expect("大压缩包物化应成功");

        assert_eq!(
            fs::metadata(workspace.root.join("big/large.log"))
                .expect("应读取大条目")
                .len(),
            large_entry.len() as u64
        );
        for index in 0..small_names.len() {
            assert!(
                workspace
                    .root
                    .join(format!("big/many/entry-{index:03}.log"))
                    .is_file(),
                "第 {index} 个条目应被完整展开"
            );
        }
        assert!(
            workspace.warnings.is_empty(),
            "不应出现预算或条目数降级警告：{:?}",
            workspace.warnings
        );
    }

    /// 验证大嵌套容器经 scratch 流式解压，产物完整且不留暂存文件。
    #[test]
    fn nested_container_streams_through_scratch_file() {
        let dir = isolated_test_dir("workspace-nested-large");
        let inner_content = vec![b'z'; 512 * 1024];
        let mut inner_cursor = std::io::Cursor::new(Vec::new());
        {
            let mut inner_writer = ZipWriter::new(&mut inner_cursor);
            inner_writer
                .start_file("inner.log", SimpleFileOptions::default())
                .expect("应写入内层条目");
            inner_writer
                .write_all(&inner_content)
                .expect("应写入内层条目内容");
            inner_writer.finish().expect("应完成内层 zip");
        }
        let inner_bytes = inner_cursor.into_inner();
        let archive = write_test_zip(
            &dir.join("outer.zip"),
            &[("nested/inner.zip", inner_bytes.as_slice())],
        );

        let workspace = materialize_sources(
            &[archive],
            &LoaderConfig::default(),
            &ArchivePasswordStore::default(),
            &test_cancellation(),
            None,
        )
        .expect("大嵌套容器物化应成功");

        assert_eq!(
            fs::metadata(workspace.root.join("outer/nested/inner.log"))
                .expect("应读取嵌套大条目")
                .len(),
            inner_content.len() as u64
        );
        assert!(
            !workspace.root.join(".argus-scratch").exists(),
            "scratch 暂存目录不应留在最终产物中"
        );
    }

    /// 验证只含单个文件的 zip 提升为顶层文件根，不再套标签目录。
    #[test]
    fn single_file_archive_promotes_to_workspace_root() {
        let dir = isolated_test_dir("workspace-promote");
        let archive = write_test_zip(&dir.join("app.zip"), &[("server.log", b"hello".as_slice())]);

        let workspace = materialize_sources(
            &[archive],
            &LoaderConfig::default(),
            &ArchivePasswordStore::default(),
            &test_cancellation(),
            None,
        )
        .expect("单文件 zip 物化应成功");

        assert!(workspace.root.join("server.log").is_file());
        assert!(!workspace.root.join("app").exists(), "标签目录应被移除");
        assert_eq!(workspace.roots.len(), 1);
        assert_eq!(workspace.roots[0].path, workspace.root.join("server.log"));
        assert_eq!(
            fs::read(workspace.root.join("server.log")).expect("应读取解压文件"),
            b"hello"
        );
    }

    /// 验证两个单文件 zip 的内部文件同名时，后者在扩展名前追加序号。
    #[test]
    fn single_file_archive_collision_appends_suffix_before_extension() {
        let dir = isolated_test_dir("workspace-promote-collision");
        let first = write_test_zip(&dir.join("one.zip"), &[("app.log", b"one".as_slice())]);
        let second = write_test_zip(&dir.join("two.zip"), &[("app.log", b"two".as_slice())]);

        let workspace = materialize_sources(
            &[first, second],
            &LoaderConfig::default(),
            &ArchivePasswordStore::default(),
            &test_cancellation(),
            None,
        )
        .expect("同名单文件 zip 物化应成功");

        assert_eq!(
            fs::read(workspace.root.join("app.log")).expect("应读取第一个文件"),
            b"one"
        );
        assert_eq!(
            fs::read(workspace.root.join("app (2).log")).expect("应读取去重后的第二个文件"),
            b"two"
        );
    }

    /// 验证压缩包内含目录或多个文件时保持标签目录布局。
    #[test]
    fn multi_entry_archive_keeps_label_directory() {
        let dir = isolated_test_dir("workspace-promote-multi");
        let archive = write_test_zip(
            &dir.join("bundle.zip"),
            &[("a.log", b"a".as_slice()), ("b.log", b"b".as_slice())],
        );

        let workspace = materialize_sources(
            &[archive],
            &LoaderConfig::default(),
            &ArchivePasswordStore::default(),
            &test_cancellation(),
            None,
        )
        .expect("多文件 zip 物化应成功");

        assert!(workspace.root.join("bundle/a.log").is_file());
        assert_eq!(workspace.roots[0].path, workspace.root.join("bundle"));
    }

    /// 验证密码解锁追加物化同样对单文件压缩包做顶层提升。
    #[test]
    fn append_materialize_promotes_single_file_archive() {
        let dir = isolated_test_dir("workspace-promote-append");
        fs::create_dir_all(&dir).expect("应创建测试目录");
        let workspace_root = dir.join("workdir");
        fs::create_dir_all(&workspace_root).expect("应创建工作目录");
        let archive = write_test_zip(
            &dir.join("locked.zip"),
            &[("unlocked.log", b"data".as_slice())],
        );

        let root_info = append_materialize_archive(
            &workspace_root,
            "locked",
            &archive,
            &LoaderConfig::default(),
            &ArchivePasswordStore::default(),
            &test_cancellation(),
        )
        .expect("追加物化应成功");

        assert!(workspace_root.join("unlocked.log").is_file());
        assert!(!workspace_root.join("locked").exists());
        assert_eq!(root_info.path, workspace_root.join("unlocked.log"));
    }

    /// 验证取消令牌触发时整个物化失败且不留工作目录。
    #[test]
    fn materialize_cancelled_aborts_and_cleans_up() {
        let dir = isolated_test_dir("workspace-cancel");
        let source = write_test_file(&dir, "app.log", "line\n");
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let result = materialize_sources(
            &[source],
            &LoaderConfig::default(),
            &ArchivePasswordStore::default(),
            &cancellation,
            None,
        );

        assert!(result.is_err());
    }

    /// 验证两个同名来源根的顶层目录会自动消歧。
    #[test]
    fn duplicate_root_labels_are_disambiguated() {
        let dir = isolated_test_dir("workspace-dupe");
        let first_dir = dir.join("one");
        let second_dir = dir.join("two");
        let first = write_test_file(&first_dir, "app.log", "1");
        let second = write_test_file(&second_dir, "app.log", "2");

        let workspace = materialize_sources(
            &[first, second],
            &LoaderConfig::default(),
            &ArchivePasswordStore::default(),
            &test_cancellation(),
            None,
        )
        .expect("同名来源物化应成功");

        assert!(workspace.root.join("app/app.log").exists());
        assert!(workspace.root.join("app (2)/app.log").exists());
    }
}
