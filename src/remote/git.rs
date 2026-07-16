//! 文件职责：使用内置 libgit2 实现 Git 仓库的只读缓存、浏览、预览与下载。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-16
//! 作者：Argus 开发团队
//! 主要功能：维护按链接隔离的裸仓库缓存，显式完成 HTTPS/SSH 鉴权，并从 tree/blob 读取文件。

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use anyhow::{Context as AnyhowContext, Result, anyhow, bail};
use async_channel::Sender;
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use git2::{
    AutotagOption, CertificateCheckStatus, Cred, FetchOptions, FetchPrune, ObjectType,
    RemoteCallbacks, Repository,
};

use crate::config::paths::argus_git_repositories_dir;
use crate::remote::connection::{ConnectionNodeId, GitLinkConfig};
use crate::remote::remote_file::{
    FilePreviewContent, REMOTE_FILE_PREVIEW_MAX_FILE_SIZE, REMOTE_FILE_PREVIEW_MAX_READ,
    RemoteFileCommand, RemoteFileEntry, RemoteFileEntryKind, RemoteFileEvent, RepositoryVersion,
    RepositoryVersionKind, local_download_path, preview_content_from_bytes, remote_child_path,
    remote_file_name, resolve_remote_path, send_event_blocking, send_file_preview_loaded,
    send_operation_failed,
};

/// 远程分支在裸仓库中的本地命名空间。
const GIT_REMOTE_BRANCH_PREFIX: &str = "refs/remotes/origin/";
/// Git 标签命名空间。
const GIT_TAG_PREFIX: &str = "refs/tags/";
/// Git tree 中目录、链接和子模块的文件模式。
const GIT_MODE_TREE: i32 = 0o040000;
const GIT_MODE_SYMLINK: i32 = 0o120000;
const GIT_MODE_SUBMODULE: i32 = 0o160000;
/// 每个链接独立的缓存互斥锁表，避免并发 fetch、清理和对象读取相互破坏。
static GIT_LINK_LOCKS: OnceLock<Mutex<BTreeMap<ConnectionNodeId, Arc<Mutex<()>>>>> =
    OnceLock::new();

/// 返回指定 Git 链接的进程内共享缓存锁。
fn git_link_lock(link_id: ConnectionNodeId) -> Arc<Mutex<()>> {
    GIT_LINK_LOCKS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(link_id)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// 在后台等待链接会话释放缓存锁后删除裸仓库，避免连接编辑或删除动作阻塞 UI 线程。
///
/// 参数：
/// - `cache_root`：与当前配置文件对应的 Git 缓存根目录；
/// - `link_id`：待清理的链接编号；
/// - `expected_remote_url`：编辑 URL 时传旧地址，仅删除仍属于旧远端的缓存；删除链接时传空。
///
/// 返回值：线程成功启动时返回可等待的句柄；线程内结果表示实际缓存清理是否成功。
pub(crate) fn schedule_git_cache_removal_at(
    cache_root: PathBuf,
    link_id: ConnectionNodeId,
    expected_remote_url: Option<String>,
) -> Result<std::thread::JoinHandle<Result<()>>> {
    std::thread::Builder::new()
        .name(format!("argus-git-cache-cleanup-{link_id}"))
        .spawn(move || {
            let link_lock = git_link_lock(link_id);
            let _guard = link_lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            remove_git_cache_without_lock(&cache_root, link_id, expected_remote_url.as_deref())
        })
        .context("无法启动 Git 缓存清理线程")
}

/// 在调用方已持有链接锁时清理缓存；可按 origin URL 跳过已经由新会话重建的仓库。
fn remove_git_cache_without_lock(
    cache_root: &Path,
    link_id: ConnectionNodeId,
    expected_remote_url: Option<&str>,
) -> Result<()> {
    let cache_path = cache_root.join(format!("{link_id}.git"));
    if !cache_path.exists() {
        return Ok(());
    }
    if let Some(expected_remote_url) = expected_remote_url
        && let Ok(repository) = Repository::open_bare(&cache_path)
    {
        let cached_remote_url = repository
            .find_remote("origin")
            .ok()
            .and_then(|remote| remote.url().ok().map(str::to_string));
        if cached_remote_url.as_deref() != Some(expected_remote_url) {
            // 新 URL 的 worker 可能先取得锁并重建了缓存；延迟清理不得删除新会话数据。
            return Ok(());
        }
    }
    std::fs::remove_dir_all(&cache_path)
        .with_context(|| format!("无法删除 Git 缓存 {}", cache_path.display()))?;
    Ok(())
}

/// 启动时清理指定缓存根目录中不再对应任何 Git 链接的数字命名缓存。
///
/// 参数：`cache_root` 为与当前设置文件同根派生的缓存目录，测试可传入临时目录；
/// `valid_link_ids` 为当前配置仍然保留的 Git 链接编号。未知文件一律保留。
pub(crate) fn cleanup_orphaned_git_caches(
    cache_root: &Path,
    valid_link_ids: &BTreeSet<ConnectionNodeId>,
) -> Result<()> {
    if !cache_root.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(cache_root)
        .with_context(|| format!("无法扫描 Git 缓存目录 {}", cache_root.display()))?
    {
        let entry = entry.context("无法读取 Git 缓存目录项")?;
        if !entry
            .file_type()
            .context("无法读取 Git 缓存目录项类型")?
            .is_dir()
        {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().to_string();
        let Some(link_id) = file_name
            .strip_suffix(".git")
            .and_then(|value| value.parse::<ConnectionNodeId>().ok())
        else {
            continue;
        };
        if !valid_link_ids.contains(&link_id) {
            std::fs::remove_dir_all(entry.path())
                .with_context(|| format!("无法删除孤立 Git 缓存 {}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// Git worker 主流程：更新裸仓库缓存后，从当前引用对应的提交树串行处理只读命令。
pub(super) fn run_git_worker(
    session_id: usize,
    link_id: ConnectionNodeId,
    git: GitLinkConfig,
    trusted_fingerprint: Option<String>,
    command_receiver: mpsc::Receiver<RemoteFileCommand>,
    event_sender: Sender<RemoteFileEvent>,
) -> Result<()> {
    let link_lock = git_link_lock(link_id);
    let _cache_guard = link_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let repository = open_git_cache(link_id, &git.url)?;
    let had_cache = !collect_git_versions(&repository, None).0.is_empty();
    let mut accepted_fingerprint = trusted_fingerprint;
    let initial_fetch = fetch_repository(
        &repository,
        &git,
        &mut accepted_fingerprint,
        session_id,
        &command_receiver,
        &event_sender,
    );
    if !had_cache && let Err(error) = &initial_fetch {
        return Err(anyhow!("首次获取 Git 仓库失败：{error}"));
    }
    let (mut versions, mut default_version) = collect_git_versions(
        &repository,
        initial_fetch
            .as_ref()
            .ok()
            .and_then(|default| default.as_deref()),
    );
    if versions.is_empty() {
        return Err(initial_fetch
            .err()
            .map(|error| anyhow!("首次获取 Git 仓库失败：{error}"))
            .unwrap_or_else(|| anyhow!("Git 仓库没有可浏览的远程分支或标签")));
    }
    let mut warning = initial_fetch.err().and_then(|error| {
        had_cache.then(|| format!("更新 Git 仓库失败，当前正在浏览本地缓存：{error}"))
    });
    let mut selected_version = default_version
        .take()
        .or_else(|| versions.first().map(|version| version.id.clone()))
        .ok_or_else(|| anyhow!("Git 仓库没有可浏览版本"))?;
    let mut current_dir = "/".to_string();
    let entries = read_git_directory(&repository, &selected_version, &current_dir)?;
    send_event_blocking(
        &event_sender,
        RemoteFileEvent::Connected {
            session_id,
            current_dir: current_dir.clone(),
            entries,
        },
    );
    send_git_versions(
        session_id,
        &versions,
        &selected_version,
        warning.take(),
        &event_sender,
    );

    while let Ok(command) = command_receiver.recv() {
        match command {
            RemoteFileCommand::TrustHostKey | RemoteFileCommand::RejectHostKey => {}
            RemoteFileCommand::LoadDirectory(path) => {
                let target_path = resolve_remote_path(&current_dir, &path)
                    .and_then(|path| normalize_repository_path(&path));
                match target_path.and_then(|target| {
                    read_git_directory(&repository, &selected_version, &target)
                        .map(|entries| (target, entries))
                }) {
                    Ok((target, entries)) => {
                        current_dir = target;
                        send_event_blocking(
                            &event_sender,
                            RemoteFileEvent::DirectoryLoaded {
                                session_id,
                                current_dir: current_dir.clone(),
                                entries,
                            },
                        );
                    }
                    Err(error) => send_operation_failed(&event_sender, session_id, error),
                }
            }
            RemoteFileCommand::Refresh => {
                let previous_version = selected_version.clone();
                let fetch_result = fetch_repository(
                    &repository,
                    &git,
                    &mut accepted_fingerprint,
                    session_id,
                    &command_receiver,
                    &event_sender,
                );
                let (next_versions, next_default) = collect_git_versions(
                    &repository,
                    fetch_result
                        .as_ref()
                        .ok()
                        .and_then(|default| default.as_deref()),
                );
                if next_versions.is_empty() {
                    send_operation_failed(
                        &event_sender,
                        session_id,
                        fetch_result
                            .err()
                            .unwrap_or_else(|| anyhow!("Git 缓存没有可浏览版本")),
                    );
                    continue;
                }
                versions = next_versions;
                let version_still_exists = versions
                    .iter()
                    .any(|version| version.id == previous_version);
                selected_version = if version_still_exists {
                    previous_version
                } else {
                    next_default
                        .or_else(|| versions.first().map(|version| version.id.clone()))
                        .expect("非空版本集合必须存在首项")
                };
                if !version_still_exists {
                    current_dir = "/".to_string();
                }
                let mut message = match fetch_result {
                    Ok(_) if version_still_exists => Some("Git 仓库已刷新".to_string()),
                    Ok(_) => Some("当前版本已被删除，已回退到默认分支".to_string()),
                    Err(error) => Some(format!("更新失败，继续浏览本地缓存：{error}")),
                };
                let (next_dir, entries, directory_was_reset) = match read_git_refresh_directory(
                    &repository,
                    &selected_version,
                    &current_dir,
                ) {
                    Ok(result) => result,
                    Err(error) => {
                        // 引用已经由 fetch 移动，不能继续展示旧提交的目录项；清空到根目录保持状态一致。
                        current_dir = "/".to_string();
                        send_git_versions(
                            session_id,
                            &versions,
                            &selected_version,
                            message,
                            &event_sender,
                        );
                        send_event_blocking(
                            &event_sender,
                            RemoteFileEvent::DirectoryLoaded {
                                session_id,
                                current_dir: current_dir.clone(),
                                entries: Vec::new(),
                            },
                        );
                        send_operation_failed(&event_sender, session_id, error);
                        continue;
                    }
                };
                current_dir = next_dir;
                if directory_was_reset {
                    message = Some(match message {
                        Some(message) => format!("{message}；当前目录已不存在，已回到仓库根目录"),
                        None => "当前目录已不存在，已回到仓库根目录".to_string(),
                    });
                }
                send_git_versions(
                    session_id,
                    &versions,
                    &selected_version,
                    message,
                    &event_sender,
                );
                send_event_blocking(
                    &event_sender,
                    RemoteFileEvent::DirectoryLoaded {
                        session_id,
                        current_dir: current_dir.clone(),
                        entries,
                    },
                );
            }
            RemoteFileCommand::SwitchRepositoryVersion { version_id } => {
                if !versions.iter().any(|version| version.id == version_id) {
                    send_operation_failed(
                        &event_sender,
                        session_id,
                        anyhow!("所选 Git 分支或标签已经不存在"),
                    );
                    continue;
                }
                match read_git_directory(&repository, &version_id, "/") {
                    Ok(entries) => {
                        selected_version = version_id;
                        current_dir = "/".to_string();
                        send_git_versions(
                            session_id,
                            &versions,
                            &selected_version,
                            None,
                            &event_sender,
                        );
                        send_event_blocking(
                            &event_sender,
                            RemoteFileEvent::DirectoryLoaded {
                                session_id,
                                current_dir: current_dir.clone(),
                                entries,
                            },
                        );
                    }
                    Err(error) => send_operation_failed(&event_sender, session_id, error),
                }
            }
            RemoteFileCommand::DownloadFile {
                remote_path,
                local_path,
            } => {
                let result =
                    download_git_file(&repository, &selected_version, &remote_path, &local_path)
                        .map(|_| format!("已下载 {}", remote_file_name(&remote_path)));
                send_git_operation_result(session_id, result, &event_sender);
            }
            RemoteFileCommand::DownloadFiles { entries, local_dir } => {
                let result =
                    download_git_files(&repository, &selected_version, &entries, &local_dir);
                send_git_operation_result(session_id, result, &event_sender);
            }
            RemoteFileCommand::ReadFileContent { remote_path } => send_file_preview_loaded(
                session_id,
                read_git_file_preview(&repository, &selected_version, &remote_path),
                &event_sender,
            ),
            RemoteFileCommand::UploadFiles { .. }
            | RemoteFileCommand::Rename { .. }
            | RemoteFileCommand::Delete { .. } => send_operation_failed(
                &event_sender,
                session_id,
                anyhow!("Git 仓库是只读的，不允许写入、重命名或删除"),
            ),
            RemoteFileCommand::Disconnect => {
                send_event_blocking(
                    &event_sender,
                    RemoteFileEvent::Disconnected {
                        session_id,
                        message: "Git 仓库会话已关闭".to_string(),
                    },
                );
                return Ok(());
            }
        }
    }

    send_event_blocking(
        &event_sender,
        RemoteFileEvent::Disconnected {
            session_id,
            message: "Git 仓库会话已关闭".to_string(),
        },
    );
    Ok(())
}

/// 打开链接对应的裸仓库缓存；URL 变化时删除旧缓存，避免不同远端对象混入同一命名空间。
fn open_git_cache(link_id: ConnectionNodeId, remote_url: &str) -> Result<Repository> {
    let cache_root = argus_git_repositories_dir();
    std::fs::create_dir_all(&cache_root)
        .with_context(|| format!("无法创建 Git 缓存目录 {}", cache_root.display()))?;
    let cache_path = cache_root.join(format!("{link_id}.git"));
    if cache_path.exists() {
        let repository = Repository::open_bare(&cache_path)
            .with_context(|| format!("无法打开 Git 裸仓库缓存 {}", cache_path.display()))?;
        let cached_url = repository
            .find_remote("origin")
            .ok()
            .and_then(|remote| remote.url().ok().map(str::to_string));
        if cached_url.as_deref() == Some(remote_url) {
            return Ok(repository);
        }
        drop(repository);
        std::fs::remove_dir_all(&cache_path)
            .with_context(|| format!("无法清理 URL 已变化的 Git 缓存 {}", cache_path.display()))?;
    }
    let repository = Repository::init_bare(&cache_path)
        .with_context(|| format!("无法初始化 Git 裸仓库缓存 {}", cache_path.display()))?;
    repository
        .remote("origin", remote_url)
        .context("无法创建 Git origin 远端")?;
    Ok(repository)
}

/// 从远端获取全部分支和标签，并启用 prune；返回服务端默认分支的本地 ref。
fn fetch_repository(
    repository: &Repository,
    config: &GitLinkConfig,
    trusted_fingerprint: &mut Option<String>,
    session_id: usize,
    command_receiver: &mpsc::Receiver<RemoteFileCommand>,
    event_sender: &Sender<RemoteFileEvent>,
) -> Result<Option<String>> {
    let mut remote = repository
        .find_remote("origin")
        .context("Git 缓存缺少 origin 远端")?;
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(|_url, username_from_url, _allowed| {
        if config
            .url
            .trim()
            .to_ascii_lowercase()
            .starts_with("https://")
        {
            let username = config
                .username
                .as_deref()
                .or(username_from_url)
                .ok_or_else(|| git2::Error::from_str("HTTPS 用户名未配置"))?;
            let token = config
                .access_token
                .as_deref()
                .ok_or_else(|| git2::Error::from_str("HTTPS 访问令牌未配置"))?;
            Cred::userpass_plaintext(username, token)
        } else {
            let username = config
                .username
                .as_deref()
                .or(username_from_url)
                .ok_or_else(|| git2::Error::from_str("SSH 用户名未配置"))?;
            let private_key_path = config
                .private_key_path
                .as_deref()
                .ok_or_else(|| git2::Error::from_str("SSH 私钥未配置"))?;
            Cred::ssh_key(
                username,
                None,
                Path::new(private_key_path),
                config.private_key_passphrase.as_deref(),
            )
        }
    });
    callbacks.certificate_check(|certificate, host| {
        let Some(host_key) = certificate.as_hostkey() else {
            // HTTPS 证书不在应用层放宽校验，交还 libgit2 使用系统信任链判定。
            return Ok(CertificateCheckStatus::CertificatePassthrough);
        };
        let hash = host_key
            .hash_sha256()
            .ok_or_else(|| git2::Error::from_str("Git SSH 服务端未提供 SHA256 主机指纹"))?;
        let fingerprint = format!("SHA256:{}", STANDARD_NO_PAD.encode(hash));
        match trusted_fingerprint.as_deref() {
            Some(expected) if expected == fingerprint => Ok(CertificateCheckStatus::CertificateOk),
            Some(_) => Err(git2::Error::from_str(
                "Git SSH 主机指纹发生变化，已阻止连接",
            )),
            None => {
                let (_, port) =
                    git_ssh_host_and_port(&config.url).unwrap_or_else(|| (host.to_string(), 22));
                send_event_blocking(
                    event_sender,
                    RemoteFileEvent::HostKeyVerification {
                        session_id,
                        host: host.to_string(),
                        port,
                        fingerprint: fingerprint.clone(),
                    },
                );
                loop {
                    match command_receiver.recv() {
                        Ok(RemoteFileCommand::TrustHostKey) => {
                            *trusted_fingerprint = Some(fingerprint);
                            return Ok(CertificateCheckStatus::CertificateOk);
                        }
                        Ok(RemoteFileCommand::RejectHostKey) => {
                            return Err(git2::Error::from_str("用户拒绝信任 Git SSH 主机指纹"));
                        }
                        Ok(RemoteFileCommand::Disconnect) | Err(_) => {
                            return Err(git2::Error::from_str("Git 连接已取消"));
                        }
                        Ok(_) => {}
                    }
                }
            }
        }
    });
    let mut last_progress_percent = None;
    callbacks.transfer_progress(|progress| {
        let total = progress.total_objects();
        let received = progress.received_objects();
        let percent = received.saturating_mul(100).checked_div(total).unwrap_or(0);
        if last_progress_percent != Some(percent) {
            last_progress_percent = Some(percent);
            send_event_blocking(
                event_sender,
                RemoteFileEvent::TransferProgress {
                    session_id,
                    message: format!("正在获取 Git 对象：{received}/{total}（{percent}%）"),
                },
            );
        }
        true
    });
    let mut fetch_options = FetchOptions::new();
    fetch_options
        .remote_callbacks(callbacks)
        .prune(FetchPrune::On)
        .download_tags(AutotagOption::All);
    remote
        .fetch(
            &[
                "+refs/heads/*:refs/remotes/origin/*",
                "+refs/tags/*:refs/tags/*",
            ],
            Some(&mut fetch_options),
            None,
        )
        .context("无法从 Git 远端获取分支和标签")?;
    let default_branch = remote
        .default_branch()
        .ok()
        .and_then(|name| name.as_str().ok().map(str::to_string))
        .and_then(|name| name.strip_prefix("refs/heads/").map(str::to_string))
        .map(|name| format!("{GIT_REMOTE_BRANCH_PREFIX}{name}"));
    Ok(default_branch)
}

/// 从缓存引用构造版本列表，并确保默认分支在远程分支组首位、标签位于所有分支之后。
fn collect_git_versions(
    repository: &Repository,
    remote_default: Option<&str>,
) -> (Vec<RepositoryVersion>, Option<String>) {
    let mut branches = collect_references(repository, "refs/remotes/origin/*")
        .into_iter()
        .filter(|name| name != "refs/remotes/origin/HEAD")
        .map(|id| RepositoryVersion {
            label: id
                .strip_prefix(GIT_REMOTE_BRANCH_PREFIX)
                .unwrap_or(&id)
                .to_string(),
            id,
            kind: RepositoryVersionKind::GitBranch,
        })
        .collect::<Vec<_>>();
    branches.sort_by(|left, right| left.label.cmp(&right.label));
    let default = remote_default
        .filter(|candidate| branches.iter().any(|version| version.id == *candidate))
        .map(str::to_string)
        .or_else(|| {
            ["main", "master"]
                .into_iter()
                .map(|name| format!("{GIT_REMOTE_BRANCH_PREFIX}{name}"))
                .find(|candidate| branches.iter().any(|version| version.id == *candidate))
        })
        .or_else(|| branches.first().map(|version| version.id.clone()));
    if let Some(default_id) = default.as_deref()
        && let Some(index) = branches.iter().position(|version| version.id == default_id)
    {
        let mut default_branch = branches.remove(index);
        default_branch.label.push_str("（默认分支）");
        branches.insert(0, default_branch);
    }
    let mut tags = collect_references(repository, "refs/tags/*")
        .into_iter()
        .filter(|id| {
            repository
                .find_reference(id)
                .and_then(|reference| reference.peel_to_commit())
                .is_ok()
        })
        .map(|id| RepositoryVersion {
            label: id.strip_prefix(GIT_TAG_PREFIX).unwrap_or(&id).to_string(),
            id,
            kind: RepositoryVersionKind::GitTag,
        })
        .collect::<Vec<_>>();
    tags.sort_by(|left, right| left.label.cmp(&right.label));
    branches.extend(tags);
    (branches, default)
}

/// 返回匹配 glob 的直接或符号引用名称，忽略损坏或非 UTF-8 引用。
fn collect_references(repository: &Repository, glob: &str) -> Vec<String> {
    repository
        .references_glob(glob)
        .into_iter()
        .flatten()
        .filter_map(|reference| reference.ok())
        .filter_map(|reference| reference.name().ok().map(str::to_string))
        .collect()
}

/// 发送仓库版本列表与当前版本，Git 的输入展示使用版本标签。
fn send_git_versions(
    session_id: usize,
    versions: &[RepositoryVersion],
    selected_version: &str,
    message: Option<String>,
    event_sender: &Sender<RemoteFileEvent>,
) {
    let input_value = versions
        .iter()
        .find(|version| version.id == selected_version)
        .map(|version| version.label.clone())
        .unwrap_or_else(|| selected_version.to_string());
    send_event_blocking(
        event_sender,
        RemoteFileEvent::RepositoryVersionsLoaded {
            session_id,
            versions: versions.to_vec(),
            selected_version: selected_version.to_string(),
            input_value,
            message,
        },
    );
}

/// 读取刷新后的当前目录；目录已被新提交删除时自动回退根目录。
///
/// 参数：`repository`、`version` 和 `current_dir` 分别表示裸仓库、刷新后的引用及刷新前目录。
/// 返回值：实际目录、目录项以及是否发生根目录回退；连根目录也无法读取时返回错误。
fn read_git_refresh_directory(
    repository: &Repository,
    version: &str,
    current_dir: &str,
) -> Result<(String, Vec<RemoteFileEntry>, bool)> {
    match read_git_directory(repository, version, current_dir) {
        Ok(entries) => Ok((current_dir.to_string(), entries, false)),
        Err(current_error) if current_dir != "/" => read_git_directory(repository, version, "/")
            .map(|entries| ("/".to_string(), entries, true))
            .with_context(|| format!("刷新后当前目录不可用：{current_error}")),
        Err(error) => Err(error),
    }
}

/// 读取指定 Git 版本与绝对仓库路径对应的直接子项，不跟随符号链接或子模块。
fn read_git_directory(
    repository: &Repository,
    version: &str,
    path: &str,
) -> Result<Vec<RemoteFileEntry>> {
    let normalized = normalize_repository_path(path)?;
    let tree = git_tree_at_path(repository, version, &normalized)?;
    let mut entries = tree
        .iter()
        .map(|entry| {
            let name = String::from_utf8_lossy(entry.name_bytes()).into_owned();
            let mode = entry.filemode();
            let kind = match mode & 0o170000 {
                GIT_MODE_TREE => RemoteFileEntryKind::Directory,
                GIT_MODE_SYMLINK => RemoteFileEntryKind::Symlink,
                GIT_MODE_SUBMODULE => RemoteFileEntryKind::Other,
                _ if entry.kind() == Some(ObjectType::Blob) => RemoteFileEntryKind::RegularFile,
                _ => RemoteFileEntryKind::Other,
            };
            let size = (kind == RemoteFileEntryKind::RegularFile)
                .then(|| {
                    repository
                        .find_blob(entry.id())
                        .ok()
                        .map(|blob| blob.size() as u64)
                })
                .flatten();
            RemoteFileEntry {
                path: remote_child_path(&normalized, &name),
                name,
                kind,
                size,
                // Git tree 不记录逐文件修改时间。
                mtime: None,
                permissions: Some((mode as u32) & 0o777),
            }
        })
        .collect::<Vec<_>>();
    entries.sort_by_cached_key(|entry| {
        (
            entry.kind != RemoteFileEntryKind::Directory,
            entry.name.to_ascii_lowercase(),
            entry.name.clone(),
        )
    });
    Ok(entries)
}

/// 返回指定版本中路径对应的 tree；标签通过 peel_to_commit 统一解析到提交树。
fn git_tree_at_path<'repo>(
    repository: &'repo Repository,
    version: &str,
    path: &str,
) -> Result<git2::Tree<'repo>> {
    let commit = repository
        .find_reference(version)
        .with_context(|| format!("Git 版本不存在：{version}"))?
        .peel_to_commit()
        .with_context(|| format!("Git 版本无法解析到提交：{version}"))?;
    let root = commit.tree().context("无法读取 Git 提交树")?;
    let relative = repository_relative_path(path)?;
    if relative.is_empty() {
        return Ok(root);
    }
    root.get_path(Path::new(&relative))
        .with_context(|| format!("Git 目录不存在：{path}"))?
        .to_object(repository)
        .context("无法读取 Git tree 对象")?
        .peel_to_tree()
        .with_context(|| format!("Git 路径不是目录：{path}"))
}

/// 读取普通 blob；符号链接、目录和子模块即使具有对象 ID 也必须拒绝。
fn read_git_blob<'repo>(
    repository: &'repo Repository,
    version: &str,
    path: &str,
) -> Result<git2::Blob<'repo>> {
    let normalized = normalize_repository_path(path)?;
    let relative = repository_relative_path(&normalized)?;
    if relative.is_empty() {
        bail!("仓库根目录不是普通文件");
    }
    let commit = repository
        .find_reference(version)
        .with_context(|| format!("Git 版本不存在：{version}"))?
        .peel_to_commit()
        .with_context(|| format!("Git 版本无法解析到提交：{version}"))?;
    let tree = commit.tree().context("无法读取 Git 提交树")?;
    let entry = tree
        .get_path(Path::new(&relative))
        .with_context(|| format!("Git 文件不存在：{path}"))?;
    if entry.kind() != Some(ObjectType::Blob) || entry.filemode() & 0o170000 == GIT_MODE_SYMLINK {
        bail!("仅支持读取 Git 普通文件：{path}");
    }
    repository
        .find_blob(entry.id())
        .with_context(|| format!("无法读取 Git blob：{path}"))
}

/// 把仓库地址栏输入归一化成以 `/` 开头的路径，并阻止 `..` 越过仓库根目录。
fn normalize_repository_path(path: &str) -> Result<String> {
    let trimmed = path.trim();
    let absolute = if trimmed.is_empty() || trimmed == "." {
        "/".to_string()
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    };
    let mut components = Vec::new();
    for component in absolute.split('/') {
        match component {
            "" | "." => {}
            ".." => bail!("仓库路径不能越过浏览根目录"),
            value if value.contains('\\') => bail!("仓库路径不能包含反斜杠"),
            value => components.push(value),
        }
    }
    Ok(if components.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", components.join("/"))
    })
}

/// 将绝对仓库路径转换成 libgit2 tree 使用的相对路径。
fn repository_relative_path(path: &str) -> Result<String> {
    Ok(normalize_repository_path(path)?
        .trim_start_matches('/')
        .to_string())
}

/// 下载一个 Git blob 到用户选择的本地路径。
fn download_git_file(
    repository: &Repository,
    version: &str,
    remote_path: &str,
    local_path: &Path,
) -> Result<()> {
    let blob = read_git_blob(repository, version, remote_path)?;
    let mut local = File::create(local_path)
        .with_context(|| format!("无法创建本地文件 {}", local_path.display()))?;
    local
        .write_all(blob.content())
        .with_context(|| format!("下载 Git 文件失败：{remote_path}"))
}

/// 下载多个 Git 普通文件；目录、符号链接和子模块均在写入前拒绝。
fn download_git_files(
    repository: &Repository,
    version: &str,
    entries: &[RemoteFileEntry],
    local_dir: &Path,
) -> Result<String> {
    if entries.is_empty() {
        bail!("未选择要下载的文件");
    }
    if !local_dir.is_dir() {
        bail!("请选择有效的本地目录");
    }
    let targets = entries
        .iter()
        .map(|entry| {
            if !entry.kind.is_regular_file() {
                bail!("仅支持下载普通文件：{}", entry.name);
            }
            Ok((entry, local_download_path(local_dir, &entry.name)?))
        })
        .collect::<Result<Vec<_>>>()?;
    for (entry, local_path) in targets {
        download_git_file(repository, version, &entry.path, &local_path)?;
    }
    Ok(format!("已下载 {} 个文件", entries.len()))
}

/// 读取 Git blob 用于预览；先执行 2 MiB 总大小校验，再最多回传 512 KiB。
fn read_git_file_preview(
    repository: &Repository,
    version: &str,
    remote_path: &str,
) -> (String, FilePreviewContent) {
    let file_name = remote_file_name(remote_path);
    let blob = match read_git_blob(repository, version, remote_path) {
        Ok(blob) => blob,
        Err(error) => return (file_name, FilePreviewContent::Error(error.to_string())),
    };
    if blob.size() as u64 > REMOTE_FILE_PREVIEW_MAX_FILE_SIZE {
        return (
            file_name,
            FilePreviewContent::Error("文件超过 2 MiB，无法预览".to_string()),
        );
    }
    let truncated = blob.size() > REMOTE_FILE_PREVIEW_MAX_READ;
    let bytes = blob.content()[..blob.size().min(REMOTE_FILE_PREVIEW_MAX_READ)].to_vec();
    (file_name, preview_content_from_bytes(bytes, truncated))
}

/// 将 Git 下载结果转换为统一成功/失败事件。
fn send_git_operation_result(
    session_id: usize,
    result: Result<String>,
    event_sender: &Sender<RemoteFileEvent>,
) {
    match result {
        Ok(message) => send_event_blocking(
            event_sender,
            RemoteFileEvent::OperationSucceeded {
                session_id,
                message,
            },
        ),
        Err(error) => send_operation_failed(event_sender, session_id, error),
    }
}

/// 解析 Git SSH URL 中的主机与端口，兼容 `ssh://` 和 scp-like 两种格式。
fn git_ssh_host_and_port(url: &str) -> Option<(String, u16)> {
    if url.trim().to_ascii_lowercase().starts_with("ssh://") {
        let parsed = url::Url::parse(url).ok()?;
        return Some((parsed.host_str()?.to_string(), parsed.port().unwrap_or(22)));
    }
    let authority = url.split_once(':')?.0;
    let host = authority.rsplit_once('@')?.1;
    Some((host.to_string(), 22))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::paths::temporary_test_dir;

    /// 使用 libgit2 在临时裸仓库中构造提交、分支、标签和特殊条目夹具。
    fn repository_fixture() -> (tempfile::TempDir, Repository) {
        let temp_dir = temporary_test_dir("git-fixture");
        let repository = Repository::init_bare(temp_dir.path()).expect("应初始化裸仓库");
        let signature =
            git2::Signature::now("Argus Test", "argus@example.com").expect("应创建测试签名");

        let empty_tree_id = repository
            .treebuilder(None)
            .unwrap()
            .write()
            .expect("应写入空 tree");
        let empty_tree = repository.find_tree(empty_tree_id).unwrap();
        let base_commit = repository
            .commit(None, &signature, &signature, "base", &empty_tree, &[])
            .expect("应写入基础提交");
        drop(empty_tree);

        let text_blob = repository.blob(b"hello from git\n").unwrap();
        let binary_blob = repository.blob(b"\0binary").unwrap();
        let symlink_blob = repository.blob(b"README.md").unwrap();
        let child_blob = repository.blob(b"child").unwrap();
        let mut child_builder = repository.treebuilder(None).unwrap();
        child_builder
            .insert("child.txt", child_blob, 0o100644)
            .unwrap();
        let child_tree = child_builder.write().unwrap();
        drop(child_builder);

        let mut root_builder = repository.treebuilder(None).unwrap();
        root_builder
            .insert("README.md", text_blob, 0o100644)
            .unwrap();
        root_builder
            .insert("binary.dat", binary_blob, 0o100644)
            .unwrap();
        root_builder.insert("docs", child_tree, 0o040000).unwrap();
        root_builder
            .insert("latest", symlink_blob, 0o120000)
            .unwrap();
        root_builder
            .insert("vendor", base_commit, 0o160000)
            .unwrap();
        let root_tree_id = root_builder.write().unwrap();
        drop(root_builder);
        let root_tree = repository.find_tree(root_tree_id).unwrap();
        let base = repository.find_commit(base_commit).unwrap();
        let commit_id = repository
            .commit(
                None,
                &signature,
                &signature,
                "fixture",
                &root_tree,
                &[&base],
            )
            .expect("应写入夹具提交");
        drop(base);
        drop(root_tree);
        repository
            .reference("refs/remotes/origin/main", commit_id, true, "test")
            .unwrap();
        repository
            .reference("refs/remotes/origin/feature", base_commit, true, "test")
            .unwrap();
        repository
            .reference("refs/tags/v1", commit_id, true, "test")
            .unwrap();
        let commit_object = repository
            .find_object(commit_id, Some(ObjectType::Commit))
            .unwrap();
        repository
            .tag("v2", &commit_object, &signature, "annotated", false)
            .unwrap();
        drop(commit_object);
        (temp_dir, repository)
    }

    /// 验证仓库路径归一化始终限制在根目录内。
    #[test]
    fn repository_path_normalization_rejects_parent_escape() {
        assert_eq!(
            normalize_repository_path("src/main.rs").unwrap(),
            "/src/main.rs"
        );
        assert_eq!(normalize_repository_path("/").unwrap(), "/");
        assert!(normalize_repository_path("../secret").is_err());
        assert!(normalize_repository_path("/src/../secret").is_err());
    }

    /// 验证两种 Git SSH URL 都能提取主机和端口。
    #[test]
    fn git_ssh_url_host_and_port_supports_both_forms() {
        assert_eq!(
            git_ssh_host_and_port("ssh://git@example.com:2222/repo.git"),
            Some(("example.com".to_string(), 2222))
        );
        assert_eq!(
            git_ssh_host_and_port("git@example.com:repo.git"),
            Some(("example.com".to_string(), 22))
        );
    }

    /// 验证默认分支排序、远程分支去重和轻量/附注标签都能解析到提交树。
    #[test]
    fn git_versions_order_default_branch_before_tags_and_peel_annotated_tags() {
        let (_temp_dir, repository) = repository_fixture();
        let (versions, default) =
            collect_git_versions(&repository, Some("refs/remotes/origin/main"));

        assert_eq!(default.as_deref(), Some("refs/remotes/origin/main"));
        assert_eq!(versions[0].id, "refs/remotes/origin/main");
        assert!(versions[0].label.contains("默认分支"));
        let first_tag = versions
            .iter()
            .position(|version| version.kind == RepositoryVersionKind::GitTag)
            .unwrap();
        assert!(
            versions[..first_tag]
                .iter()
                .all(|version| version.kind == RepositoryVersionKind::GitBranch)
        );
        assert!(
            !read_git_directory(&repository, "refs/tags/v2", "/")
                .unwrap()
                .is_empty()
        );
    }

    /// 验证 tree/blob 浏览区分目录、普通文件、链接和子模块，并按规则处理二进制预览。
    #[test]
    fn git_tree_listing_and_preview_respect_special_entry_boundaries() {
        let (_temp_dir, repository) = repository_fixture();
        let entries = read_git_directory(&repository, "refs/remotes/origin/main", "/").unwrap();

        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.name == "docs")
                .unwrap()
                .kind,
            RemoteFileEntryKind::Directory
        );
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.name == "README.md")
                .unwrap()
                .kind,
            RemoteFileEntryKind::RegularFile
        );
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.name == "latest")
                .unwrap()
                .kind,
            RemoteFileEntryKind::Symlink
        );
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.name == "vendor")
                .unwrap()
                .kind,
            RemoteFileEntryKind::Other
        );
        assert!(read_git_blob(&repository, "refs/remotes/origin/main", "/latest").is_err());
        let (_, preview) =
            read_git_file_preview(&repository, "refs/remotes/origin/main", "/binary.dat");
        assert!(matches!(preview, FilePreviewContent::Binary));
    }

    /// 后台缓存清理必须立即返回给调用方，并在活动会话释放链接锁后再删除目录。
    #[test]
    fn scheduled_cache_removal_waits_for_session_lock_off_ui_thread() {
        let temp_dir = temporary_test_dir("git-cache-removal");
        let link_id = usize::MAX - 41;
        let cache_path = temp_dir.path().join(format!("{link_id}.git"));
        Repository::init_bare(&cache_path).expect("应创建待清理缓存");

        let link_lock = git_link_lock(link_id);
        let guard = link_lock.lock().expect("测试链接锁不应中毒");
        let cleanup = schedule_git_cache_removal_at(temp_dir.path().to_path_buf(), link_id, None)
            .expect("应成功调度后台清理");
        assert!(cache_path.exists(), "持锁期间后台线程不得提前删除缓存");

        drop(guard);
        cleanup
            .join()
            .expect("后台清理线程不应 panic")
            .expect("后台清理应成功");
        assert!(!cache_path.exists());
    }

    /// URL 编辑后的延迟清理不得删除已经按新远端重建的缓存。
    #[test]
    fn conditional_cache_removal_preserves_cache_for_new_remote() {
        let temp_dir = temporary_test_dir("git-cache-preserve");
        let link_id = usize::MAX - 42;
        let cache_path = temp_dir.path().join(format!("{link_id}.git"));
        let repository = Repository::init_bare(&cache_path).expect("应创建缓存仓库");
        repository
            .remote("origin", "https://new.example/repository.git")
            .expect("应创建新远端");
        drop(repository);

        remove_git_cache_without_lock(
            temp_dir.path(),
            link_id,
            Some("https://old.example/repository.git"),
        )
        .expect("条件清理应成功跳过新缓存");

        assert!(cache_path.exists());
    }

    /// 刷新后当前目录被删除时应回退根目录，不继续展示旧提交的目录项。
    #[test]
    fn git_refresh_directory_falls_back_to_root_when_directory_disappears() {
        let (_temp_dir, repository) = repository_fixture();
        let replacement = repository
            .refname_to_id("refs/remotes/origin/feature")
            .expect("应读取不含 docs 的替代提交");
        repository
            .find_reference("refs/remotes/origin/main")
            .expect("应读取主分支引用")
            .set_target(replacement, "simulate refresh")
            .expect("应移动主分支引用");

        let (current_dir, entries, was_reset) =
            read_git_refresh_directory(&repository, "refs/remotes/origin/main", "/docs")
                .expect("应回退到可读取的根目录");

        assert_eq!(current_dir, "/");
        assert!(entries.is_empty());
        assert!(was_reset);
    }
}
