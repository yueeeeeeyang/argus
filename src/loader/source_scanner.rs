//! 文件职责：实现来源树全量扫描器，为 AI 会话准备和目录树完整初始化提供统一的树构建能力。
//! 创建日期：2026-07-17
//! 修改日期：2026-09-07
//! 作者：Argus 开发团队
//! 主要功能：一次遍历本地目录、一次枚举每个归档容器、按组单遍批量展开嵌套归档，并在结束时批量构建来源注册表。

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};

use crate::config::LoaderConfig;
use crate::loader::archive::adapter::{
    ArchiveEntryInfo, read_archive_entry_bytes_with_passwords, stream_archive_entry_with_passwords,
};
use crate::loader::archive::detector::{
    ArchiveFormat, detect_archive_format, detect_archive_format_by_name,
};
use crate::loader::archive::{
    ArchivePasswordKey, ArchivePasswordStore, archive_registry, find_archive_password_error,
};
use crate::loader::{
    SourceId, SourceKind, SourceLocation, SourceMetadata, SourceRegistry, SourceTreeNode,
};
use crate::utils::path::{display_name, display_path, normalize_archive_entry_path};

/// 来源树扫描结果；注册表已经完成一次性索引构建，可直接生成会话快照或回填主窗口。
#[derive(Debug)]
pub(crate) struct SourceTreeScanResult {
    /// 完整扫描后的来源注册表；未在当前授权范围内的根保持原状。
    pub registry: SourceRegistry,
    /// 可容忍的目录、归档和符号链接读取警告。
    pub warnings: Vec<String>,
}

/// 来源树扫描进度；`current` 描述正在处理的目录或压缩包，供界面展示当前处理位置。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SourceTreeScanProgress {
    /// 已生成的来源节点数，单调不减。
    pub scanned: usize,
    /// 正在处理的目录、压缩包或来源根展示文本；扫描准备阶段为空。
    pub current: String,
}

/// 来源树全量扫描器，一次构建完整来源树，不调用 UI 展开或渐进探测逻辑。
pub(crate) struct SourceTreeScanner<'a> {
    /// 扫描前的来源树，只用于取得根位置、复用稳定 ID 和保留未选根。
    original_registry: &'a SourceRegistry,
    /// 目录、符号链接和嵌套归档深度配置。
    config: LoaderConfig,
    /// 当前进程已授权的归档密码快照。
    archive_passwords: ArchivePasswordStore,
    /// 用户主动停止时由所有目录和归档边界检查的取消令牌。
    cancellation: tokio_util::sync::CancellationToken,
    /// 可选的进度上报通道；开始处理目录、归档前后发送进度快照，仅完整加载入口使用。
    progress: Option<std::sync::mpsc::Sender<SourceTreeScanProgress>>,
    /// 正在处理的目录、压缩包或来源根展示文本，随进度快照一起发送。
    current_item: String,
    /// 已有来源身份到稳定 ID 的映射和新增 ID 分配状态。
    stable_ids: StableSourceIds,
    /// 已访问真实目录，跟随符号链接时用于阻止循环。
    visited_directories: HashSet<PathBuf>,
    /// 去重后的安全警告。
    warnings: BTreeSet<String>,
    /// 父节点先于子节点的最终节点序列；结束时只重建一次索引。
    ordered_nodes: Vec<SourceTreeNode>,
}

impl<'a> SourceTreeScanner<'a> {
    /// 为来源树副本创建独立扫描器；构造阶段不访问文件系统。
    pub(crate) fn new(
        original_registry: &'a SourceRegistry,
        config: LoaderConfig,
        archive_passwords: ArchivePasswordStore,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self {
            original_registry,
            config,
            archive_passwords,
            cancellation,
            progress: None,
            current_item: String::new(),
            stable_ids: StableSourceIds::from_registry(original_registry),
            visited_directories: HashSet::new(),
            warnings: BTreeSet::new(),
            ordered_nodes: Vec::new(),
        }
    }

    /// 从用户给定路径列表一次性完整构建整棵来源树；每个路径合成一个深度为 0 的扫描根。
    ///
    /// 参数说明：
    /// - `paths`：待加载的本地文件、目录或压缩包路径。
    /// - `config`：目录、符号链接和嵌套归档深度配置。
    /// - `archive_passwords`：当前进程已授权的归档密码快照。
    /// - `cancellation`：新加载请求到来时用于中断本次扫描的取消令牌。
    /// - `progress`：可选进度通道，在开始处理每个目录、压缩包前后发送进度快照。
    ///
    /// 说明：整树替换场景没有既有注册表，内部以空注册表创建扫描器，稳定 ID 自然从 1 开始分配；
    /// 目录递归与每个压缩包的内容枚举（含嵌套，深度上限沿用 `config.max_archive_depth`）在一次调用内完成。
    pub(crate) fn scan_paths(
        paths: Vec<PathBuf>,
        config: LoaderConfig,
        archive_passwords: ArchivePasswordStore,
        cancellation: tokio_util::sync::CancellationToken,
        progress: Option<std::sync::mpsc::Sender<SourceTreeScanProgress>>,
    ) -> Result<SourceTreeScanResult> {
        let original_registry = SourceRegistry::new();
        let mut scanner =
            SourceTreeScanner::new(&original_registry, config, archive_passwords, cancellation);
        scanner.progress = progress;
        scanner.scan_path_roots(paths)
    }

    /// 以指定归档节点为根重新扫描其指向的压缩包，生成以该节点为根的独立注册表。
    ///
    /// 与整树扫描的降级策略不同：密码错误等枚举失败直接作为 `Err` 上抛，
    /// 供界面区分密码错误再次弹窗，而不是降级为警告节点。
    pub(crate) fn scan_archive_subtree(
        node: &SourceTreeNode,
        config: LoaderConfig,
        archive_passwords: ArchivePasswordStore,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<SourceTreeScanResult> {
        let original_registry = SourceRegistry::new();
        let mut scanner =
            SourceTreeScanner::new(&original_registry, config, archive_passwords, cancellation);
        scanner.scan_archive_subtree_root(node)?;
        Ok(SourceTreeScanResult {
            registry: SourceRegistry::from_ordered_nodes(scanner.ordered_nodes),
            warnings: scanner.warnings.into_iter().collect(),
        })
    }

    /// 完整扫描指定根；其它根的已有子树保持不变，避免单根智能分析破坏主窗口其它来源。
    pub(crate) fn scan(mut self, selected_root_ids: &[SourceId]) -> Result<SourceTreeScanResult> {
        self.ensure_not_cancelled()?;
        let selected_roots = selected_root_ids.iter().copied().collect::<HashSet<_>>();
        if selected_roots.is_empty() {
            bail!("来源树扫描至少需要一个来源根");
        }

        // 未授权根不会参与扫描，但它们的 ID 必须提前保留，防止相同真实路径被选中根误复用。
        for source_id in self.original_registry.tree_order_source_ids() {
            let Some(root_id) = self.original_registry.root_id_for(*source_id) else {
                continue;
            };
            if !selected_roots.contains(&root_id) {
                self.stable_ids.reserve(*source_id);
            }
        }

        for root_id in self.original_registry.root_ids() {
            self.ensure_not_cancelled()?;
            if selected_roots.contains(root_id) {
                self.scan_selected_root(*root_id)?;
            } else {
                self.copy_existing_subtree(*root_id);
            }
        }

        Ok(SourceTreeScanResult {
            registry: SourceRegistry::from_ordered_nodes(self.ordered_nodes),
            warnings: self.warnings.into_iter().collect(),
        })
    }

    /// 逐个扫描用户给定路径；每个路径边界都检查取消令牌，保证新加载请求能及时中断在途扫描。
    fn scan_path_roots(mut self, paths: Vec<PathBuf>) -> Result<SourceTreeScanResult> {
        self.ensure_not_cancelled()?;
        if paths.is_empty() {
            bail!("来源树扫描至少需要一个来源路径");
        }
        for path in paths {
            self.ensure_not_cancelled()?;
            let label = display_name(&path);
            self.begin_progress_item(display_path(&path));
            self.scan_local_path(None, 0, label, path, None)?;
            self.report_progress();
        }
        Ok(SourceTreeScanResult {
            registry: SourceRegistry::from_ordered_nodes(self.ordered_nodes),
            warnings: self.warnings.into_iter().collect(),
        })
    }

    /// 以归档节点为根严格重扫其子树；密码错误必须上抛，由调用方决定再次弹窗或放弃。
    fn scan_archive_subtree_root(&mut self, node: &SourceTreeNode) -> Result<()> {
        self.ensure_not_cancelled()?;
        let nested_format = match &node.kind {
            SourceKind::Archive(format) => *format,
            _ => bail!("来源树子树重试目标不是压缩包节点"),
        };
        match &node.location {
            SourceLocation::LocalPath(path) => {
                // 本地压缩包节点：严格枚举一次，失败直接上抛而不生成降级节点。
                let entries = self.list_local_archive(path, nested_format)?;
                self.ensure_not_cancelled()?;
                self.emit_local_archive_tree(
                    None,
                    0,
                    node.label.clone(),
                    path.clone(),
                    nested_format,
                    None,
                    entries,
                )
            }
            SourceLocation::ArchiveEntry {
                archive_path,
                root_format,
                container_entries,
                entry_path,
                format,
                archive_depth,
            } => {
                // 嵌套压缩包节点：沿容器链路逐层读出该压缩包字节，再严格枚举其内容。
                let bytes = read_archive_entry_bytes_with_passwords(
                    archive_path,
                    *root_format,
                    container_entries,
                    entry_path,
                    &self.archive_passwords,
                )?;
                self.ensure_not_cancelled()?;
                let mut nested_container_entries = container_entries.clone();
                nested_container_entries.push(entry_path.clone());
                let source_label = nested_container_entries.join("!/");
                let password_key =
                    ArchivePasswordKey::new(archive_path.clone(), &nested_container_entries);
                let reader_len = bytes.len() as u64;
                let mut reader = Cursor::new(bytes.as_slice());
                let entries = archive_registry().list_entries_from_reader_with_password_context(
                    nested_format,
                    &mut reader,
                    reader_len,
                    &source_label,
                    self.archive_passwords.get(&password_key),
                    password_key,
                )?;
                self.ensure_not_cancelled()?;
                let context = ArchiveContainerContext {
                    archive_path: archive_path.clone(),
                    root_format: *root_format,
                    container_entries: container_entries.clone(),
                    format: *format,
                    archive_depth: *archive_depth,
                };
                self.emit_nested_archive_tree(
                    None,
                    0,
                    node.label.clone(),
                    entry_path,
                    node.metadata.size,
                    nested_format,
                    node.location.clone(),
                    &context,
                    entries,
                    &bytes,
                )
            }
        }
    }

    /// 根据根节点的真实位置重新发现其完整内容，并始终保留根 ID。
    fn scan_selected_root(&mut self, root_id: SourceId) -> Result<()> {
        let root = self
            .original_registry
            .node(root_id)
            .cloned()
            .ok_or_else(|| anyhow!("来源树根不存在"))?;
        match &root.location {
            SourceLocation::LocalPath(path) => {
                self.scan_local_root(&root, path.clone())?;
            }
            SourceLocation::ArchiveEntry {
                archive_path,
                root_format,
                ..
            } if root.parent_id.is_none() => {
                // 单文件归档根的 UI 位置已经折叠到内部条目；必须从真实外层归档重新枚举。
                self.scan_local_archive(
                    None,
                    root.depth,
                    root.label,
                    archive_path.clone(),
                    *root_format,
                    Some(root.id),
                )?;
            }
            SourceLocation::ArchiveEntry { .. } => {
                self.warnings.insert(format!(
                    "来源根“{}”不是本地根，已保留现有来源结构",
                    root.label
                ));
                self.copy_existing_subtree(root.id);
            }
        }
        Ok(())
    }

    /// 扫描本地目录、普通文件或归档根；保持根 ID 稳定。
    fn scan_local_root(&mut self, root: &SourceTreeNode, path: PathBuf) -> Result<()> {
        self.scan_local_path(None, root.depth, root.label.clone(), path, Some(root.id))
    }

    /// 扫描任意本地路径并生成对应来源节点；`preferred_id` 仅在重建既有根时保留稳定 ID。
    fn scan_local_path(
        &mut self,
        parent_id: Option<SourceId>,
        depth: usize,
        label: String,
        path: PathBuf,
        preferred_id: Option<SourceId>,
    ) -> Result<()> {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                self.push_node(
                    parent_id,
                    depth,
                    label.clone(),
                    SourceKind::Unsupported("来源不可读".to_string()),
                    SourceLocation::LocalPath(path),
                    SourceMetadata {
                        message: Some("无法读取该来源".to_string()),
                        ..SourceMetadata::default()
                    },
                    preferred_id,
                    &[],
                );
                self.warnings.insert(format!(
                    "无法读取来源根“{}”：{}",
                    label,
                    safe_io_error(&error)
                ));
                return Ok(());
            }
        };

        if metadata.file_type().is_symlink() && !self.config.follow_symlinks {
            self.push_node(
                parent_id,
                depth,
                label,
                SourceKind::Unsupported("符号链接".to_string()),
                SourceLocation::LocalPath(path),
                SourceMetadata {
                    children_loaded: true,
                    message: Some("已跳过符号链接，避免目录循环".to_string()),
                    ..SourceMetadata::default()
                },
                preferred_id,
                &[],
            );
            return Ok(());
        }

        let followed_metadata = if metadata.file_type().is_symlink() {
            fs::metadata(&path).unwrap_or(metadata)
        } else {
            metadata
        };
        if followed_metadata.is_dir() {
            self.remember_directory(&path)?;
            let directory_id = self.push_node(
                parent_id,
                depth,
                label,
                SourceKind::Directory,
                SourceLocation::LocalPath(path.clone()),
                SourceMetadata {
                    children_loaded: true,
                    ..SourceMetadata::default()
                },
                preferred_id,
                &[],
            );
            self.scan_local_directory(directory_id, &path, depth.saturating_add(1))?;
            return Ok(());
        }

        if let Some(format) = detect_archive_format(&path) {
            if format.is_supported() {
                return self.scan_local_archive(
                    parent_id,
                    depth,
                    label,
                    path,
                    format,
                    preferred_id,
                );
            }
            self.push_node(
                parent_id,
                depth,
                label,
                SourceKind::Unsupported(format.label().to_string()),
                SourceLocation::LocalPath(path),
                SourceMetadata {
                    size: Some(followed_metadata.len()),
                    children_loaded: true,
                    message: Some("该压缩格式当前不可展开".to_string()),
                    ..SourceMetadata::default()
                },
                preferred_id,
                &[],
            );
            return Ok(());
        }

        self.push_node(
            parent_id,
            depth,
            label,
            SourceKind::LogFile,
            SourceLocation::LocalPath(path),
            SourceMetadata {
                size: Some(followed_metadata.len()),
                children_loaded: true,
                ..SourceMetadata::default()
            },
            preferred_id,
            &[],
        );
        Ok(())
    }

    /// 一次读取本地目录直接子项；子目录递归时不触发任何 UI 索引或压缩包预探测。
    fn scan_local_directory(
        &mut self,
        parent_id: SourceId,
        path: &Path,
        depth: usize,
    ) -> Result<()> {
        self.ensure_not_cancelled()?;
        self.begin_progress_item(display_path(path));
        let read_dir = match fs::read_dir(path) {
            Ok(read_dir) => read_dir,
            Err(error) => {
                self.warnings.insert(format!(
                    "无法读取目录“{}”：{}",
                    display_name(path),
                    safe_io_error(&error)
                ));
                return Ok(());
            }
        };
        let mut entries = Vec::new();
        for entry in read_dir {
            self.ensure_not_cancelled()?;
            let Ok(entry) = entry else {
                self.warnings
                    .insert(format!("目录“{}”包含无法读取的目录项", display_name(path)));
                continue;
            };
            let entry_path = entry.path();
            let Ok(link_metadata) = fs::symlink_metadata(&entry_path) else {
                self.warnings.insert(format!(
                    "无法读取目录项“{}”的元数据",
                    display_name(&entry_path)
                ));
                continue;
            };
            let is_symlink = link_metadata.file_type().is_symlink();
            let metadata = if is_symlink && self.config.follow_symlinks {
                fs::metadata(&entry_path).unwrap_or(link_metadata)
            } else {
                link_metadata
            };
            let label = display_name(&entry_path);
            entries.push(LocalScanEntry {
                sort_key: label.to_lowercase(),
                label,
                path: entry_path,
                metadata,
                is_symlink,
            });
        }
        entries.sort_by(|left, right| {
            let left_group = usize::from(!left.metadata.is_dir());
            let right_group = usize::from(!right.metadata.is_dir());
            left_group
                .cmp(&right_group)
                .then_with(|| left.sort_key.cmp(&right.sort_key))
                .then_with(|| left.label.cmp(&right.label))
        });

        for entry in entries {
            self.ensure_not_cancelled()?;
            if entry.is_symlink && !self.config.follow_symlinks {
                self.push_node(
                    Some(parent_id),
                    depth,
                    entry.label,
                    SourceKind::Unsupported("符号链接".to_string()),
                    SourceLocation::LocalPath(entry.path),
                    SourceMetadata {
                        children_loaded: true,
                        message: Some("已跳过符号链接，避免目录循环".to_string()),
                        ..SourceMetadata::default()
                    },
                    None,
                    &[],
                );
                continue;
            }
            if entry.metadata.is_dir() {
                if !self.remember_directory(&entry.path)? {
                    self.push_node(
                        Some(parent_id),
                        depth,
                        entry.label,
                        SourceKind::Unsupported("目录循环".to_string()),
                        SourceLocation::LocalPath(entry.path),
                        SourceMetadata {
                            children_loaded: true,
                            message: Some("已跳过重复真实目录，避免符号链接循环".to_string()),
                            ..SourceMetadata::default()
                        },
                        None,
                        &[],
                    );
                    continue;
                }
                let directory_path = entry.path;
                let directory_id = self.push_node(
                    Some(parent_id),
                    depth,
                    entry.label,
                    SourceKind::Directory,
                    SourceLocation::LocalPath(directory_path.clone()),
                    SourceMetadata {
                        children_loaded: true,
                        ..SourceMetadata::default()
                    },
                    None,
                    &[],
                );
                self.scan_local_directory(directory_id, &directory_path, depth.saturating_add(1))?;
                continue;
            }

            if let Some(format) = detect_archive_format(&entry.path) {
                if format.is_supported() {
                    self.scan_local_archive(
                        Some(parent_id),
                        depth,
                        entry.label,
                        entry.path,
                        format,
                        None,
                    )?;
                } else {
                    self.push_node(
                        Some(parent_id),
                        depth,
                        entry.label,
                        SourceKind::Unsupported(format.label().to_string()),
                        SourceLocation::LocalPath(entry.path),
                        SourceMetadata {
                            size: Some(entry.metadata.len()),
                            children_loaded: true,
                            message: Some("该压缩格式当前不可展开".to_string()),
                            ..SourceMetadata::default()
                        },
                        None,
                        &[],
                    );
                }
                continue;
            }

            self.push_node(
                Some(parent_id),
                depth,
                entry.label,
                SourceKind::LogFile,
                SourceLocation::LocalPath(entry.path),
                SourceMetadata {
                    size: Some(entry.metadata.len()),
                    children_loaded: true,
                    ..SourceMetadata::default()
                },
                None,
                &[],
            );
        }
        self.report_progress();
        Ok(())
    }

    /// 枚举本地归档一次并直接从扁平条目表构建完整虚拟目录树。
    #[allow(clippy::too_many_arguments)]
    fn scan_local_archive(
        &mut self,
        parent_id: Option<SourceId>,
        depth: usize,
        label: String,
        archive_path: PathBuf,
        format: ArchiveFormat,
        preferred_id: Option<SourceId>,
    ) -> Result<()> {
        self.ensure_not_cancelled()?;
        self.begin_progress_item(display_path(&archive_path));
        let compressed_location = SourceLocation::LocalPath(archive_path.clone());
        let entries = match self.list_local_archive(&archive_path, format) {
            Ok(entries) => entries,
            Err(error) => {
                let size = fs::metadata(&archive_path)
                    .ok()
                    .map(|metadata| metadata.len());
                let (metadata, reason) = archive_scan_failure(&error, size);
                self.warnings
                    .insert(format!("无法枚举归档“{label}”：{reason}"));
                self.push_node(
                    parent_id,
                    depth,
                    label,
                    SourceKind::Archive(format),
                    compressed_location,
                    metadata,
                    preferred_id,
                    &[],
                );
                self.report_progress();
                return Ok(());
            }
        };
        self.ensure_not_cancelled()?;
        let result = self.emit_local_archive_tree(
            parent_id,
            depth,
            label,
            archive_path,
            format,
            preferred_id,
            entries,
        );
        self.report_progress();
        result
    }

    /// 本地归档枚举成功后构建其子树；恰好一个普通文件时折叠为单文件叶子。
    #[allow(clippy::too_many_arguments)]
    fn emit_local_archive_tree(
        &mut self,
        parent_id: Option<SourceId>,
        depth: usize,
        label: String,
        archive_path: PathBuf,
        format: ArchiveFormat,
        preferred_id: Option<SourceId>,
        entries: Vec<ArchiveEntryInfo>,
    ) -> Result<()> {
        let compressed_location = SourceLocation::LocalPath(archive_path.clone());
        let tree = ArchiveTreeNode::from_entries(entries);
        if let Some(single_file) = tree.single_plain_file() {
            let location = SourceLocation::ArchiveEntry {
                archive_path,
                root_format: format,
                container_entries: Vec::new(),
                entry_path: single_file.full_path.clone(),
                format,
                archive_depth: 0,
            };
            self.push_node(
                parent_id,
                depth,
                label,
                SourceKind::SingleFileArchive(format),
                location,
                SourceMetadata {
                    size: single_file.size,
                    children_loaded: true,
                    ..SourceMetadata::default()
                },
                preferred_id,
                &[compressed_location],
            );
            return Ok(());
        }

        let archive_size = fs::metadata(&archive_path)
            .ok()
            .map(|metadata| metadata.len());
        let archive_id = self.push_node(
            parent_id,
            depth,
            label,
            SourceKind::Archive(format),
            compressed_location,
            SourceMetadata {
                size: archive_size,
                children_loaded: true,
                ..SourceMetadata::default()
            },
            preferred_id,
            &[],
        );
        let context = ArchiveContainerContext {
            archive_path,
            root_format: format,
            container_entries: Vec::new(),
            format,
            archive_depth: 0,
        };
        self.emit_archive_children(
            archive_id,
            depth.saturating_add(1),
            &tree,
            &context,
            &ContainerRef::Local {
                path: &context.archive_path,
            },
        )
    }

    /// 把已经枚举的归档树递归转换为来源节点；普通虚拟目录不会重新打开归档。
    ///
    /// 嵌套压缩包按组单遍批量读取（分组预算见 `NESTED_GROUP_MAX_*`），避免同一容器内
    /// 每个嵌套包都重新解析物理容器；节点发射顺序保持 `sorted_children` 不变。
    fn emit_archive_children(
        &mut self,
        parent_id: SourceId,
        depth: usize,
        tree: &ArchiveTreeNode,
        context: &ArchiveContainerContext,
        container: &ContainerRef<'_>,
    ) -> Result<()> {
        // 第一遍只分类不发射：嵌套候选按展示顺序收集，供第二遍分组批量读取。
        let mut candidates: Vec<NestedCandidate> = Vec::new();
        let mut plans = Vec::new();
        for child in tree.sorted_children() {
            if child.is_directory() {
                plans.push(ChildPlan::Directory(child));
                continue;
            }
            let Some(nested_format) = detect_archive_format_by_name(&child.name) else {
                plans.push(ChildPlan::PlainFile(child));
                continue;
            };
            if !nested_format.is_supported() {
                plans.push(ChildPlan::UnsupportedNested(child, nested_format));
                continue;
            }
            if context.archive_depth.saturating_add(1) > self.config.max_archive_depth {
                plans.push(ChildPlan::DepthExceeded(child, nested_format));
                continue;
            }
            candidates.push(NestedCandidate {
                name: child.name.clone(),
                full_path: child.full_path.clone(),
                size: child.size,
                format: nested_format,
            });
            plans.push(ChildPlan::Nested(candidates.len() - 1));
        }

        // 第二遍按计划顺序发射；遇到尚未读取的嵌套候选时，以它为起点批量读取下一组。
        let mut buffered: HashMap<String, Vec<u8>> = HashMap::new();
        let mut degraded: HashMap<String, (SourceMetadata, String)> = HashMap::new();
        for plan in plans {
            self.ensure_not_cancelled()?;
            match plan {
                ChildPlan::Directory(child) => {
                    let directory_id = self.push_node(
                        Some(parent_id),
                        depth,
                        child.name.clone(),
                        SourceKind::ArchiveDirectory,
                        archive_entry_location(context, &child.full_path),
                        SourceMetadata {
                            children_loaded: true,
                            ..SourceMetadata::default()
                        },
                        None,
                        &[],
                    );
                    self.emit_archive_children(
                        directory_id,
                        depth.saturating_add(1),
                        child,
                        context,
                        container,
                    )?;
                }
                ChildPlan::PlainFile(child) => {
                    self.push_node(
                        Some(parent_id),
                        depth,
                        child.name.clone(),
                        SourceKind::ArchiveFile,
                        archive_entry_location(context, &child.full_path),
                        SourceMetadata {
                            size: child.size,
                            children_loaded: true,
                            ..SourceMetadata::default()
                        },
                        None,
                        &[],
                    );
                }
                ChildPlan::UnsupportedNested(child, nested_format) => {
                    self.push_node(
                        Some(parent_id),
                        depth,
                        child.name.clone(),
                        SourceKind::Unsupported(nested_format.label().to_string()),
                        archive_entry_location(context, &child.full_path),
                        SourceMetadata {
                            size: child.size,
                            children_loaded: true,
                            message: Some("该嵌套压缩格式当前不可展开".to_string()),
                            ..SourceMetadata::default()
                        },
                        None,
                        &[],
                    );
                }
                ChildPlan::DepthExceeded(child, nested_format) => {
                    self.push_node(
                        Some(parent_id),
                        depth,
                        child.name.clone(),
                        SourceKind::Unsupported(format!("{} 超出深度", nested_format.label())),
                        archive_entry_location(context, &child.full_path),
                        SourceMetadata {
                            size: child.size,
                            children_loaded: true,
                            message: Some(format!(
                                "嵌套压缩包深度超过 {}，暂不展开",
                                self.config.max_archive_depth
                            )),
                            ..SourceMetadata::default()
                        },
                        None,
                        &[],
                    );
                }
                ChildPlan::Nested(index) => {
                    if !buffered.contains_key(&candidates[index].full_path)
                        && !degraded.contains_key(&candidates[index].full_path)
                    {
                        self.fetch_nested_group(
                            &candidates,
                            index,
                            context,
                            container,
                            &mut buffered,
                            &mut degraded,
                        )?;
                    }
                    let candidate = &candidates[index];
                    let location = archive_entry_location(context, &candidate.full_path);
                    if let Some((metadata, reason)) = degraded.remove(&candidate.full_path) {
                        self.warnings
                            .insert(format!("无法读取嵌套归档“{}”：{reason}", candidate.name));
                        self.push_node(
                            Some(parent_id),
                            depth,
                            candidate.name.clone(),
                            SourceKind::Archive(candidate.format),
                            location,
                            metadata,
                            None,
                            &[],
                        );
                        self.report_progress();
                        continue;
                    }
                    let bytes = buffered.remove(&candidate.full_path).ok_or_else(|| {
                        anyhow!("嵌套归档批量读取结果缺失：{}", candidate.full_path)
                    })?;
                    self.expand_nested_archive(
                        parent_id, depth, candidate, context, location, bytes,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// 在当前容器上单遍批量读取一组嵌套归档字节；未被批量访问覆盖的成员回退为逐条目链路重读。
    ///
    /// 分组预算限制同时驻留内存的嵌套包字节；批量访问因单个加密或损坏条目中止时，
    /// 未读取成功的成员逐个回退，保持逐条目降级语义不变。
    fn fetch_nested_group(
        &mut self,
        candidates: &[NestedCandidate],
        start: usize,
        context: &ArchiveContainerContext,
        container: &ContainerRef<'_>,
        buffered: &mut HashMap<String, Vec<u8>>,
        degraded: &mut HashMap<String, (SourceMetadata, String)>,
    ) -> Result<()> {
        let mut group_end = start.saturating_add(1);
        let mut group_bytes = candidates[start].size.unwrap_or(0);
        while group_end < candidates.len() {
            if group_end - start >= NESTED_GROUP_MAX_ENTRIES
                || group_bytes >= NESTED_GROUP_MAX_UNCOMPRESSED_BYTES
            {
                break;
            }
            group_bytes = group_bytes.saturating_add(candidates[group_end].size.unwrap_or(0));
            group_end = group_end.saturating_add(1);
        }
        let group = &candidates[start..group_end];
        // 批量读取是全组最耗时的操作之一，先上报首成员的完整容器链路再开始读取。
        self.begin_progress_item(nested_chain_display(context, &group[0].full_path));

        let visit_result = self.visit_nested_candidates(group, context, container, buffered);
        if visit_result.is_err() && self.cancellation.is_cancelled() {
            bail!("来源树完整扫描已取消");
        }
        // 单遍访问未覆盖的成员（条目缺失或组访问中止）逐条回退到链路重读。
        for member in group {
            if buffered.contains_key(&member.full_path) {
                continue;
            }
            self.ensure_not_cancelled()?;
            match self.read_nested_bytes_via_chain(context, member) {
                Ok(bytes) => {
                    buffered.insert(member.full_path.clone(), bytes);
                }
                Err(error) => {
                    if self.cancellation.is_cancelled() {
                        bail!("来源树完整扫描已取消");
                    }
                    degraded.insert(
                        member.full_path.clone(),
                        archive_scan_failure(&error, member.size),
                    );
                }
            }
        }
        Ok(())
    }

    /// 在当前容器上执行一次单遍批量访问，把组内嵌套归档字节写入缓冲。
    ///
    /// 嵌套容器的 `source_label` 与枚举时保持一致（GZIP 虚拟条目名由标签末段推导）；
    /// 闭包内只缓冲字节不递归展开，保证外层容器句柄释放后再深入下一层。
    fn visit_nested_candidates(
        &self,
        group: &[NestedCandidate],
        context: &ArchiveContainerContext,
        container: &ContainerRef<'_>,
        buffered: &mut HashMap<String, Vec<u8>>,
    ) -> Result<()> {
        let Some(adapter) = archive_registry().adapter_for(context.format) else {
            bail!("未注册压缩格式适配器：{:?}", context.format);
        };
        let targets: HashSet<String> = group
            .iter()
            .map(|candidate| candidate.full_path.clone())
            .collect();
        let cancellation = self.cancellation.clone();
        let mut consumer = |entry_path: &str, reader: &mut dyn Read| -> Result<()> {
            let capacity = group
                .iter()
                .find(|candidate| candidate.full_path == entry_path)
                .and_then(|candidate| candidate.size)
                .and_then(|size| usize::try_from(size).ok())
                .unwrap_or_default();
            let mut bytes = Vec::with_capacity(capacity);
            let mut chunk = [0_u8; 64 * 1024];
            loop {
                if cancellation.is_cancelled() {
                    bail!("来源树完整扫描已取消");
                }
                let read_count = reader.read(&mut chunk)?;
                if read_count == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..read_count]);
            }
            buffered.insert(normalize_archive_entry_path(entry_path), bytes);
            Ok(())
        };
        match container {
            ContainerRef::Local { path } => {
                let key = ArchivePasswordKey::root(context.archive_path.clone());
                adapter.visit_entries(
                    path,
                    &targets,
                    self.archive_passwords.get(&key),
                    &mut consumer,
                )
            }
            ContainerRef::InMemory { bytes } => {
                let key = ArchivePasswordKey::new(
                    context.archive_path.clone(),
                    &context.container_entries,
                );
                let source_label = context.container_entries.join("!/");
                let mut reader = Cursor::new(*bytes);
                adapter.visit_entries_from_reader(
                    &mut reader,
                    bytes.len() as u64,
                    &targets,
                    &source_label,
                    self.archive_passwords.get(&key),
                    &mut consumer,
                )
            }
        }
    }

    /// 沿容器链路重读一个嵌套归档的字节；仅在容器批量读取失败后逐条目回退时使用，
    /// 保证单个加密或损坏的嵌套包不影响同组其它条目。
    fn read_nested_bytes_via_chain(
        &mut self,
        context: &ArchiveContainerContext,
        candidate: &NestedCandidate,
    ) -> Result<Vec<u8>> {
        self.ensure_not_cancelled()?;
        self.begin_progress_item(nested_chain_display(context, &candidate.full_path));
        let mut bytes = Vec::with_capacity(
            candidate
                .size
                .and_then(|size| usize::try_from(size).ok())
                .unwrap_or_default(),
        );
        let cancellation = self.cancellation.clone();
        stream_archive_entry_with_passwords(
            &context.archive_path,
            context.root_format,
            &context.container_entries,
            &candidate.full_path,
            &self.archive_passwords,
            &mut |chunk| {
                if cancellation.is_cancelled() {
                    bail!("来源树完整扫描已取消");
                }
                bytes.extend_from_slice(chunk);
                Ok(())
            },
        )?;
        Ok(bytes)
    }

    /// 枚举已物化的嵌套归档字节并生成其子树；枚举失败时发射降级节点，不影响兄弟条目。
    fn expand_nested_archive(
        &mut self,
        parent_id: SourceId,
        depth: usize,
        candidate: &NestedCandidate,
        context: &ArchiveContainerContext,
        compressed_location: SourceLocation,
        bytes: Vec<u8>,
    ) -> Result<()> {
        self.ensure_not_cancelled()?;
        let mut nested_container_entries = context.container_entries.clone();
        nested_container_entries.push(candidate.full_path.clone());
        let source_label = nested_container_entries.join("!/");
        let password_key =
            ArchivePasswordKey::new(context.archive_path.clone(), &nested_container_entries);
        let reader_len = bytes.len() as u64;
        let mut reader = Cursor::new(bytes.as_slice());
        let entries = match archive_registry().list_entries_from_reader_with_password_context(
            candidate.format,
            &mut reader,
            reader_len,
            &source_label,
            self.archive_passwords.get(&password_key),
            password_key,
        ) {
            Ok(entries) => entries,
            Err(error) => {
                let (metadata, reason) = archive_scan_failure(&error, candidate.size);
                self.warnings
                    .insert(format!("无法枚举嵌套归档“{}”：{reason}", candidate.name));
                self.push_node(
                    Some(parent_id),
                    depth,
                    candidate.name.clone(),
                    SourceKind::Archive(candidate.format),
                    compressed_location,
                    metadata,
                    None,
                    &[],
                );
                self.report_progress();
                return Ok(());
            }
        };
        self.ensure_not_cancelled()?;
        let result = self.emit_nested_archive_tree(
            Some(parent_id),
            depth,
            candidate.name.clone(),
            &candidate.full_path,
            candidate.size,
            candidate.format,
            compressed_location,
            context,
            entries,
            &bytes,
        );
        self.report_progress();
        result
    }

    /// 嵌套归档枚举成功后生成其子树；恰好一个普通文件时折叠为单文件叶子。
    ///
    /// `container_bytes` 在子树展开期间保持存活，供其子级嵌套包直接从内存批量读取，
    /// 避免每深入一层都回根包重走整条容器链路。
    #[allow(clippy::too_many_arguments)]
    fn emit_nested_archive_tree(
        &mut self,
        parent_id: Option<SourceId>,
        depth: usize,
        nested_label: String,
        nested_entry_path: &str,
        nested_size: Option<u64>,
        nested_format: ArchiveFormat,
        compressed_location: SourceLocation,
        context: &ArchiveContainerContext,
        entries: Vec<ArchiveEntryInfo>,
        container_bytes: &[u8],
    ) -> Result<()> {
        let tree = ArchiveTreeNode::from_entries(entries);
        let mut nested_container_entries = context.container_entries.clone();
        nested_container_entries.push(nested_entry_path.to_string());
        let nested_depth = context.archive_depth.saturating_add(1);
        if let Some(single_file) = tree.single_plain_file() {
            let location = SourceLocation::ArchiveEntry {
                archive_path: context.archive_path.clone(),
                root_format: context.root_format,
                container_entries: nested_container_entries,
                entry_path: single_file.full_path.clone(),
                format: nested_format,
                archive_depth: nested_depth,
            };
            self.push_node(
                parent_id,
                depth,
                nested_label,
                SourceKind::SingleFileArchive(nested_format),
                location,
                SourceMetadata {
                    size: single_file.size,
                    children_loaded: true,
                    ..SourceMetadata::default()
                },
                None,
                &[compressed_location],
            );
            return Ok(());
        }

        let archive_id = self.push_node(
            parent_id,
            depth,
            nested_label,
            SourceKind::Archive(nested_format),
            compressed_location,
            SourceMetadata {
                size: nested_size,
                children_loaded: true,
                ..SourceMetadata::default()
            },
            None,
            &[],
        );
        let nested_context = ArchiveContainerContext {
            archive_path: context.archive_path.clone(),
            root_format: context.root_format,
            container_entries: nested_container_entries,
            format: nested_format,
            archive_depth: nested_depth,
        };
        self.emit_archive_children(
            archive_id,
            depth.saturating_add(1),
            &tree,
            &nested_context,
            &ContainerRef::InMemory {
                bytes: container_bytes,
            },
        )
    }

    /// 使用低层归档适配器枚举本地容器，不执行单文件预探测。
    fn list_local_archive(
        &self,
        archive_path: &Path,
        format: ArchiveFormat,
    ) -> Result<Vec<ArchiveEntryInfo>> {
        let key = ArchivePasswordKey::root(archive_path.to_path_buf());
        archive_registry().list_entries_with_password_context(
            format,
            archive_path,
            self.archive_passwords.get(&key),
            key,
            display_name(archive_path),
        )
    }

    /// 记录一个真实目录；仅在跟随符号链接时重复目录需要被当作循环阻止。
    fn remember_directory(&mut self, path: &Path) -> Result<bool> {
        if !self.config.follow_symlinks {
            return Ok(true);
        }
        let canonical = fs::canonicalize(path)
            .with_context(|| format!("无法解析目录真实路径：{}", display_name(path)))?;
        Ok(self.visited_directories.insert(canonical))
    }

    /// 插入一个节点并尽量复用相同来源位置的既有 ID、展开态和选中态。
    #[allow(clippy::too_many_arguments)]
    fn push_node(
        &mut self,
        parent_id: Option<SourceId>,
        depth: usize,
        label: String,
        kind: SourceKind,
        location: SourceLocation,
        metadata: SourceMetadata,
        preferred_id: Option<SourceId>,
        alternative_locations: &[SourceLocation],
    ) -> SourceId {
        let mut identities = Vec::with_capacity(alternative_locations.len().saturating_add(1));
        identities.push(SourceIdentity::from_location(&location));
        identities.extend(
            alternative_locations
                .iter()
                .map(SourceIdentity::from_location),
        );
        let id = self.stable_ids.take(&identities, preferred_id);
        let state = self.stable_ids.state(id);
        self.ordered_nodes.push(SourceTreeNode {
            id,
            parent_id,
            depth,
            label,
            expanded: kind.can_expand() && state.expanded,
            selected: state.selected,
            kind,
            location,
            metadata,
        });
        id
    }

    /// 原样复制一个未扫描根的已有子树，保持主窗口未授权范围完全不变。
    fn copy_existing_subtree(&mut self, source_id: SourceId) {
        let Some(node) = self.original_registry.node(source_id).cloned() else {
            return;
        };
        self.ordered_nodes.push(node);
        for child_id in self.original_registry.child_ids(source_id) {
            self.copy_existing_subtree(*child_id);
        }
    }

    /// 在高成本边界及时响应用户停止。
    fn ensure_not_cancelled(&self) -> Result<()> {
        if self.cancellation.is_cancelled() {
            bail!("来源树完整扫描已取消");
        }
        Ok(())
    }

    /// 在开始处理目录、压缩包或来源根前更新当前处理项并立即上报，
    /// 保证大目录读取、归档枚举等长耗时操作期间界面能看到正在处理的位置。
    fn begin_progress_item(&mut self, current: String) {
        self.current_item = current;
        self.report_progress();
    }

    /// 上报当前进度快照；接收端随加载任务结束释放后，发送失败可安全忽略。
    fn report_progress(&self) {
        if let Some(progress) = &self.progress {
            let _ = progress.send(SourceTreeScanProgress {
                scanned: self.ordered_nodes.len(),
                current: self.current_item.clone(),
            });
        }
    }
}

/// 本地目录中的一个已完成元数据读取的条目。
struct LocalScanEntry {
    /// 真实路径。
    path: PathBuf,
    /// 末级显示名称。
    label: String,
    /// 不区分大小写的排序键。
    sort_key: String,
    /// 已解析的文件元数据。
    metadata: fs::Metadata,
    /// 原始目录项是否为符号链接。
    is_symlink: bool,
}

/// 当前归档容器的稳定读取上下文。
struct ArchiveContainerContext {
    /// 最外层本地归档路径。
    archive_path: PathBuf,
    /// 最外层格式。
    root_format: ArchiveFormat,
    /// 到当前容器的嵌套归档条目链路。
    container_entries: Vec<String>,
    /// 当前容器格式。
    format: ArchiveFormat,
    /// 当前容器嵌套深度。
    archive_depth: usize,
}

/// 单组批量读取的嵌套压缩包数量上限，控制组内字节在内存中的驻留规模。
const NESTED_GROUP_MAX_ENTRIES: usize = 64;
/// 单组批量读取的未压缩字节预算，防止上千个大嵌套包同时驻留内存。
const NESTED_GROUP_MAX_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;

/// 当前归档容器的数据来源；根容器按本地路径读取，嵌套容器复用已物化字节，避免回链重读。
enum ContainerRef<'a> {
    /// 根容器：本地归档路径，批量访问时按需打开文件。
    Local { path: &'a Path },
    /// 嵌套容器：展开父容器时已物化的字节，子树展开期间保持存活。
    InMemory { bytes: &'a [u8] },
}

/// 等待批量读取的嵌套压缩包候选，按 `sorted_children` 顺序收集。
struct NestedCandidate {
    /// 末级显示名称。
    name: String,
    /// 当前容器内的完整规范化路径。
    full_path: String,
    /// 条目未压缩大小，供分组预算和缓冲预分配使用。
    size: Option<u64>,
    /// 嵌套压缩包格式。
    format: ArchiveFormat,
}

/// `emit_archive_children` 第一遍生成的子项处理计划，保证第二遍发射顺序与排序一致。
enum ChildPlan<'a> {
    /// 虚拟目录：发射后递归。
    Directory(&'a ArchiveTreeNode),
    /// 普通归档文件叶子。
    PlainFile(&'a ArchiveTreeNode),
    /// 不支持展开的嵌套压缩格式。
    UnsupportedNested(&'a ArchiveTreeNode, ArchiveFormat),
    /// 超出配置深度的嵌套压缩包。
    DepthExceeded(&'a ArchiveTreeNode, ArchiveFormat),
    /// 可展开嵌套压缩包在候选列表中的下标。
    Nested(usize),
}

/// 生成归档内条目的来源位置；容器链路信息全部来自当前容器上下文。
fn archive_entry_location(context: &ArchiveContainerContext, entry_path: &str) -> SourceLocation {
    SourceLocation::ArchiveEntry {
        archive_path: context.archive_path.clone(),
        root_format: context.root_format,
        container_entries: context.container_entries.clone(),
        entry_path: entry_path.to_string(),
        format: context.format,
        archive_depth: context.archive_depth,
    }
}

/// 拼接嵌套归档的完整容器链路展示文本，供进度展示定位当前读取位置。
fn nested_chain_display(context: &ArchiveContainerContext, entry_path: &str) -> String {
    std::iter::once(display_path(&context.archive_path))
        .chain(context.container_entries.iter().cloned())
        .chain(std::iter::once(entry_path.to_string()))
        .collect::<Vec<_>>()
        .join("!/")
}

/// 从单次归档枚举结果构建的内存目录树节点。
#[derive(Debug, Default)]
struct ArchiveTreeNode {
    /// 当前路径段名称；根节点为空。
    name: String,
    /// 预计算的不区分大小写排序键，避免大归档排序比较器反复分配字符串。
    sort_key: String,
    /// 当前容器内的完整规范化路径。
    full_path: String,
    /// 显式目录或因后代条目推导出的目录标记。
    is_dir: bool,
    /// 普通文件条目大小。
    size: Option<u64>,
    /// 名称到直接子项的映射。
    children: HashMap<String, ArchiveTreeNode>,
}

impl ArchiveTreeNode {
    /// 把扁平归档条目一次性转成目录树；隐式目录会在插入文件路径时自动生成。
    fn from_entries(entries: Vec<ArchiveEntryInfo>) -> Self {
        let mut root = Self {
            is_dir: true,
            ..Self::default()
        };
        for entry in entries {
            root.insert(entry);
        }
        root
    }

    /// 插入一个规范化条目，并合并重复的显式目录记录。
    fn insert(&mut self, entry: ArchiveEntryInfo) {
        let path = normalize_archive_entry_path(&entry.path);
        if path.is_empty() {
            return;
        }
        let parts = path
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        let mut current = self;
        let mut accumulated = String::new();
        for (index, part) in parts.iter().enumerate() {
            if !accumulated.is_empty() {
                accumulated.push('/');
            }
            accumulated.push_str(part);
            let is_last = index + 1 == parts.len();
            let child = current
                .children
                .entry((*part).to_string())
                .or_insert_with(|| Self {
                    name: (*part).to_string(),
                    sort_key: part.to_lowercase(),
                    full_path: accumulated.clone(),
                    ..Self::default()
                });
            if !is_last || entry.is_dir {
                child.is_dir = true;
                child.size = None;
            } else if !child.is_dir {
                child.size = entry.size;
            }
            current = child;
        }
    }

    /// 返回目录优先、名称不区分大小写排序后的直接子项。
    fn sorted_children(&self) -> Vec<&Self> {
        let mut children = self.children.values().collect::<Vec<_>>();
        children.sort_by(|left, right| {
            let left_group = usize::from(!left.is_directory());
            let right_group = usize::from(!right.is_directory());
            left_group
                .cmp(&right_group)
                .then_with(|| left.sort_key.cmp(&right.sort_key))
                .then_with(|| left.name.cmp(&right.name))
        });
        children
    }

    /// 子项存在或被显式标记时按目录处理，避免异常归档中的文件/目录冲突生成重复节点。
    fn is_directory(&self) -> bool {
        self.is_dir || !self.children.is_empty()
    }

    /// 归档恰好包含一个根层普通文件时返回该文件；嵌套归档不能折叠为日志叶子。
    fn single_plain_file(&self) -> Option<&Self> {
        if self.children.len() != 1 {
            return None;
        }
        let child = self.children.values().next()?;
        (!child.is_directory() && detect_archive_format_by_name(&child.name).is_none())
            .then_some(child)
    }
}

/// 可稳定比较的来源位置身份；不包含 UI 展开态、大小或提示文字。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum SourceIdentity {
    /// 本地文件或目录。
    Local(PathBuf),
    /// 归档内部条目。
    Archive {
        archive_path: PathBuf,
        root_format: ArchiveFormat,
        container_entries: Vec<String>,
        entry_path: String,
        format: ArchiveFormat,
    },
}

impl SourceIdentity {
    /// 从来源位置生成稳定身份，归档深度可由容器链路推导，因此不参与比较。
    fn from_location(location: &SourceLocation) -> Self {
        match location {
            SourceLocation::LocalPath(path) => Self::Local(path.clone()),
            SourceLocation::ArchiveEntry {
                archive_path,
                root_format,
                container_entries,
                entry_path,
                format,
                ..
            } => Self::Archive {
                archive_path: archive_path.clone(),
                root_format: *root_format,
                container_entries: container_entries.clone(),
                entry_path: entry_path.clone(),
                format: *format,
            },
        }
    }
}

/// 已有节点的轻量界面状态；稳定 ID 复用后可继续保留选择和展开体验。
#[derive(Clone, Copy, Debug, Default)]
struct ExistingNodeState {
    /// 节点是否展开。
    expanded: bool,
    /// 节点是否被选中。
    selected: bool,
}

/// 稳定来源 ID 分配器；同一路径重复加载时使用队列保持原根顺序。
struct StableSourceIds {
    /// 来源身份到既有 ID 队列。
    ids_by_identity: HashMap<SourceIdentity, VecDeque<SourceId>>,
    /// 既有节点状态。
    state_by_id: HashMap<SourceId, ExistingNodeState>,
    /// 已被保留或复用的 ID。
    used_ids: HashSet<SourceId>,
    /// 新节点 ID 起点。
    next_id: usize,
}

impl StableSourceIds {
    /// 从扫描前注册表建立身份索引，不访问任何来源内容。
    fn from_registry(registry: &SourceRegistry) -> Self {
        let mut ids_by_identity = HashMap::<SourceIdentity, VecDeque<SourceId>>::new();
        let mut state_by_id = HashMap::new();
        let mut next_id = 1_usize;
        for source_id in registry.tree_order_source_ids() {
            let Some(node) = registry.node(*source_id) else {
                continue;
            };
            ids_by_identity
                .entry(SourceIdentity::from_location(&node.location))
                .or_default()
                .push_back(node.id);
            state_by_id.insert(
                node.id,
                ExistingNodeState {
                    expanded: node.expanded,
                    selected: node.selected,
                },
            );
            next_id = next_id.max(node.id.0.saturating_add(1));
        }
        Self {
            ids_by_identity,
            state_by_id,
            used_ids: HashSet::new(),
            next_id,
        }
    }

    /// 提前保留未扫描子树 ID。
    fn reserve(&mut self, source_id: SourceId) {
        self.used_ids.insert(source_id);
    }

    /// 优先使用显式根 ID，其次按多个可能身份复用旧 ID，最后分配全新 ID。
    fn take(&mut self, identities: &[SourceIdentity], preferred_id: Option<SourceId>) -> SourceId {
        if let Some(preferred_id) = preferred_id {
            self.used_ids.insert(preferred_id);
            return preferred_id;
        }
        for identity in identities {
            let Some(ids) = self.ids_by_identity.get_mut(identity) else {
                continue;
            };
            while let Some(source_id) = ids.pop_front() {
                if self.used_ids.insert(source_id) {
                    return source_id;
                }
            }
        }
        loop {
            let source_id = SourceId(self.next_id);
            self.next_id = self.next_id.saturating_add(1);
            if self.used_ids.insert(source_id) {
                return source_id;
            }
        }
    }

    /// 返回既有节点状态；新节点使用收起且未选中的默认状态。
    fn state(&self, source_id: SourceId) -> ExistingNodeState {
        self.state_by_id
            .get(&source_id)
            .copied()
            .unwrap_or_default()
    }
}

/// 把操作系统错误转换为不包含真实绝对路径的简短说明。
fn safe_io_error(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => "路径不存在".to_string(),
        std::io::ErrorKind::PermissionDenied => "权限不足".to_string(),
        _ => error.kind().to_string(),
    }
}

/// 把归档错误压缩成安全说明；密码错误需要保留可操作原因，其它错误不回显真实路径。
fn archive_scan_error(error: &anyhow::Error) -> String {
    if find_archive_password_error(error).is_some() {
        "归档需要密码或密码不正确".to_string()
    } else {
        "归档格式损坏、内容不完整或当前适配器无法枚举".to_string()
    }
}

/// 归档枚举失败的降级节点元信息和警告原因；提示文案以密码标记为准，
/// 保证节点展示与"输入密码后仅重试该子树"的界面入口语义一致。
fn archive_scan_failure(error: &anyhow::Error, size: Option<u64>) -> (SourceMetadata, String) {
    let reason = archive_scan_error(error);
    let metadata = SourceMetadata {
        size,
        children_loaded: false,
        archive_password_required: find_archive_password_error(error).is_some(),
        ..SourceMetadata::default()
    };
    let message = if metadata.archive_password_required {
        "需要密码访问".to_string()
    } else {
        reason.clone()
    };
    (
        SourceMetadata {
            message: Some(message),
            ..metadata
        },
        reason,
    )
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::io::Write;

    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    use super::*;
    use crate::config::paths::temporary_test_dir;
    use crate::loader::archive::ArchivePasswordErrorKind;

    /// 构造一个尚未加载子级的本地目录根。
    fn unloaded_directory_registry(path: &Path) -> (SourceRegistry, SourceId) {
        let mut registry = SourceRegistry::new();
        let root_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: root_id,
            parent_id: None,
            depth: 0,
            label: "logs".to_string(),
            kind: SourceKind::Directory,
            location: SourceLocation::LocalPath(path.to_path_buf()),
            metadata: SourceMetadata::default(),
            selected: false,
            expanded: false,
        });
        registry.rebuild_all_indices();
        (registry, root_id)
    }

    /// 验证独立扫描器递归发现目录日志且不需要逐级调用 UI 加载器。
    #[test]
    fn scans_local_directory_into_one_complete_registry() {
        let directory = temporary_test_dir("agent-native-source-directory");
        fs::create_dir(directory.path().join("nested")).expect("应创建嵌套目录");
        fs::write(directory.path().join("nested/application.log"), "ready")
            .expect("应写入测试日志");
        let (registry, root_id) = unloaded_directory_registry(directory.path());

        let result = SourceTreeScanner::new(
            &registry,
            LoaderConfig::default(),
            ArchivePasswordStore::default(),
            tokio_util::sync::CancellationToken::new(),
        )
        .scan(&[root_id])
        .expect("独立目录扫描应成功");

        let labels = result
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|source_id| result.registry.node(*source_id))
            .map(|node| node.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(labels, vec!["logs", "nested", "application.log"]);
        assert!(
            result
                .registry
                .node(root_id)
                .unwrap()
                .metadata
                .children_loaded
        );
    }

    /// 验证单次归档条目表即可生成全部隐式目录，并保留归档根稳定 ID。
    #[test]
    fn scans_archive_entries_without_reloading_virtual_directories() {
        let directory = temporary_test_dir("agent-native-source-archive");
        let archive_path = directory.path().join("logs.zip");
        let mut writer = ZipWriter::new(fs::File::create(&archive_path).expect("应创建测试归档"));
        writer
            .start_file("app/application.log", SimpleFileOptions::default())
            .expect("应创建应用日志条目");
        writer.write_all(b"ready").expect("应写入应用日志");
        writer
            .start_file("gc/2026/gc.log", SimpleFileOptions::default())
            .expect("应创建 GC 日志条目");
        writer.write_all(b"pause").expect("应写入 GC 日志");
        writer.finish().expect("应完成测试归档");

        let mut registry = SourceRegistry::new();
        let root_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: root_id,
            parent_id: None,
            depth: 0,
            label: "logs.zip".to_string(),
            kind: SourceKind::Archive(ArchiveFormat::Zip),
            location: SourceLocation::LocalPath(archive_path),
            metadata: SourceMetadata::default(),
            selected: false,
            expanded: false,
        });
        registry.rebuild_all_indices();

        let result = SourceTreeScanner::new(
            &registry,
            LoaderConfig::default(),
            ArchivePasswordStore::default(),
            tokio_util::sync::CancellationToken::new(),
        )
        .scan(&[root_id])
        .expect("独立归档扫描应成功");

        let labels = result
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|source_id| result.registry.node(*source_id))
            .map(|node| node.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            vec!["logs.zip", "app", "application.log", "gc", "2026", "gc.log"]
        );
        assert_eq!(result.registry.root_ids(), &[root_id]);
    }

    /// 验证重新扫描后相同真实日志继续使用旧 ID，保证已打开标签和证据导航不会失效。
    #[test]
    fn preserves_existing_source_id_for_unchanged_log() {
        let directory = temporary_test_dir("agent-native-source-stable-id");
        let log_path = directory.path().join("application.log");
        fs::write(&log_path, "ready").expect("应写入测试日志");
        let (mut registry, root_id) = unloaded_directory_registry(directory.path());
        let existing_log_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: existing_log_id,
            parent_id: Some(root_id),
            depth: 1,
            label: "application.log".to_string(),
            kind: SourceKind::LogFile,
            location: SourceLocation::LocalPath(log_path),
            metadata: SourceMetadata {
                size: Some(5),
                children_loaded: true,
                ..SourceMetadata::default()
            },
            selected: true,
            expanded: false,
        });
        registry.node_mut(root_id).unwrap().metadata.children_loaded = true;
        registry.rebuild_all_indices();

        let result = SourceTreeScanner::new(
            &registry,
            LoaderConfig::default(),
            ArchivePasswordStore::default(),
            tokio_util::sync::CancellationToken::new(),
        )
        .scan(&[root_id])
        .expect("重新扫描应成功");

        let node = result
            .registry
            .node(existing_log_id)
            .expect("未变化日志必须保留原 ID");
        assert_eq!(node.label, "application.log");
        assert_eq!(result.registry.selected_id(), Some(existing_log_id));
    }

    /// 验证嵌套归档只展开到配置深度，且内部日志位置包含完整容器链路。
    #[test]
    fn scans_nested_archive_once_and_builds_container_location() {
        let directory = temporary_test_dir("agent-native-source-nested-archive");
        let mut inner_cursor = Cursor::new(Vec::new());
        {
            let mut inner_writer = ZipWriter::new(&mut inner_cursor);
            inner_writer
                .start_file("memory.log", SimpleFileOptions::default())
                .expect("应创建内层日志");
            inner_writer
                .write_all(b"memory pressure")
                .expect("应写入内层日志");
            inner_writer.finish().expect("应完成内层归档");
        }
        let outer_path = directory.path().join("outer.zip");
        let mut outer_writer =
            ZipWriter::new(fs::File::create(&outer_path).expect("应创建外层归档"));
        outer_writer
            .start_file("nested/inner.zip", SimpleFileOptions::default())
            .expect("应创建嵌套归档条目");
        outer_writer
            .write_all(inner_cursor.get_ref())
            .expect("应写入嵌套归档");
        outer_writer.finish().expect("应完成外层归档");

        let mut registry = SourceRegistry::new();
        let root_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: root_id,
            parent_id: None,
            depth: 0,
            label: "outer.zip".to_string(),
            kind: SourceKind::Archive(ArchiveFormat::Zip),
            location: SourceLocation::LocalPath(outer_path.clone()),
            metadata: SourceMetadata::default(),
            selected: false,
            expanded: false,
        });
        registry.rebuild_all_indices();

        let result = SourceTreeScanner::new(
            &registry,
            LoaderConfig::default(),
            ArchivePasswordStore::default(),
            tokio_util::sync::CancellationToken::new(),
        )
        .scan(&[root_id])
        .expect("嵌套归档扫描应成功");

        let memory_node = result
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|source_id| result.registry.node(*source_id))
            .find(|node| node.label == "inner.zip")
            .expect("单文件内层归档应折叠为可读日志节点");
        assert!(matches!(
            &memory_node.location,
            SourceLocation::ArchiveEntry {
                archive_path,
                container_entries,
                entry_path,
                format: ArchiveFormat::Zip,
                archive_depth: 1,
                ..
            } if archive_path == &outer_path
                && container_entries == &["nested/inner.zip".to_string()]
                && entry_path == "memory.log"
        ));
        assert!(matches!(
            memory_node.kind,
            SourceKind::SingleFileArchive(ArchiveFormat::Zip)
        ));
    }

    /// 验证已取消扫描不会访问来源内容。
    #[test]
    fn cancelled_scan_stops_before_source_access() {
        let directory = temporary_test_dir("agent-native-source-cancelled");
        let (registry, root_id) = unloaded_directory_registry(directory.path());
        let cancellation = tokio_util::sync::CancellationToken::new();
        cancellation.cancel();

        let error = SourceTreeScanner::new(
            &registry,
            LoaderConfig::default(),
            ArchivePasswordStore::default(),
            cancellation,
        )
        .scan(&[root_id])
        .expect_err("取消后的独立扫描必须立即停止");

        assert!(error.to_string().contains("已取消"));
    }

    /// 生成一个 AES 加密的测试 ZIP；与 zip 适配器读取路径保持同一加密特性。
    fn write_encrypted_zip(path: &Path, password: &str, entries: &[(&str, &[u8])]) {
        let mut writer = ZipWriter::new(fs::File::create(path).expect("应创建加密测试归档"));
        for (name, content) in entries {
            writer
                .start_file(
                    *name,
                    SimpleFileOptions::default()
                        .with_aes_encryption(zip::AesMode::Aes256, password),
                )
                .expect("应创建加密条目");
            writer.write_all(content).expect("应写入加密条目");
        }
        writer.finish().expect("应完成加密测试归档");
    }

    /// 收集注册表中全部节点的树序标签。
    fn tree_order_labels(registry: &SourceRegistry) -> Vec<&str> {
        registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|source_id| registry.node(*source_id))
            .map(|node| node.label.as_str())
            .collect()
    }

    /// 验证 scan_paths 从混合路径列表一次性完整建树：目录递归、压缩包枚举、单文件折叠全部在加载时完成。
    #[test]
    fn scan_paths_builds_complete_tree_from_mixed_paths() {
        let directory = temporary_test_dir("scan-paths-mixed");
        let root_label = display_name(directory.path());
        fs::create_dir(directory.path().join("subdir")).expect("应创建子目录");
        fs::write(directory.path().join("subdir/deep.log"), "deep").expect("应写入深层日志");
        fs::write(directory.path().join("alpha.log"), "alpha").expect("应写入顶层日志");
        let bundle_path = directory.path().join("bundle.zip");
        let mut bundle_writer =
            ZipWriter::new(fs::File::create(&bundle_path).expect("应创建多文件归档"));
        bundle_writer
            .start_file("logs/a.log", SimpleFileOptions::default())
            .expect("应创建归档条目 a");
        bundle_writer.write_all(b"a").expect("应写入归档条目 a");
        bundle_writer
            .start_file("logs/b.log", SimpleFileOptions::default())
            .expect("应创建归档条目 b");
        bundle_writer.write_all(b"b").expect("应写入归档条目 b");
        bundle_writer.finish().expect("应完成多文件归档");

        let extra = temporary_test_dir("scan-paths-extra");
        let standalone_path = extra.path().join("standalone.log");
        fs::write(&standalone_path, "solo").expect("应写入独立日志");
        let solo_zip_path = extra.path().join("solo.zip");
        let mut solo_writer =
            ZipWriter::new(fs::File::create(&solo_zip_path).expect("应创建单文件归档"));
        solo_writer
            .start_file("only.txt", SimpleFileOptions::default())
            .expect("应创建单文件条目");
        solo_writer.write_all(b"only").expect("应写入单文件条目");
        solo_writer.finish().expect("应完成单文件归档");

        let result = SourceTreeScanner::scan_paths(
            vec![
                directory.path().to_path_buf(),
                standalone_path,
                solo_zip_path,
            ],
            LoaderConfig::default(),
            ArchivePasswordStore::default(),
            tokio_util::sync::CancellationToken::new(),
            None,
        )
        .expect("混合路径完整扫描应成功");

        assert_eq!(
            tree_order_labels(&result.registry),
            vec![
                root_label.as_str(),
                "subdir",
                "deep.log",
                "alpha.log",
                "bundle.zip",
                "logs",
                "a.log",
                "b.log",
                "standalone.log",
                "solo.zip"
            ]
        );
        // 完整初始化语义：所有可展开节点的子级都已加载，不存在懒加载占位。
        assert!(
            result
                .registry
                .tree_order_source_ids()
                .iter()
                .all(|source_id| {
                    let node = result.registry.node(*source_id).unwrap();
                    !node.kind.can_expand() || node.metadata.children_loaded
                })
        );
        // 单文件压缩包折叠为 SingleFileArchive，位置指向内部唯一文件。
        let solo_node = result
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|source_id| result.registry.node(*source_id))
            .find(|node| node.label == "solo.zip")
            .expect("单文件压缩包根应存在");
        assert!(matches!(
            solo_node.kind,
            SourceKind::SingleFileArchive(ArchiveFormat::Zip)
        ));
        assert!(matches!(
            &solo_node.location,
            SourceLocation::ArchiveEntry { entry_path, .. } if entry_path == "only.txt"
        ));
    }

    /// 验证 scan_paths 按 max_archive_depth 展开嵌套压缩包，超限层级降级为不可展开节点。
    #[test]
    fn scan_paths_expands_nested_archives_up_to_configured_depth() {
        let directory = temporary_test_dir("scan-paths-nested-depth");
        let mut inner_cursor = Cursor::new(Vec::new());
        {
            let mut inner_writer = ZipWriter::new(&mut inner_cursor);
            inner_writer
                .start_file("deep.log", SimpleFileOptions::default())
                .expect("应创建最内层日志");
            inner_writer.write_all(b"deep").expect("应写入最内层日志");
            inner_writer.finish().expect("应完成最内层归档");
        }
        let mut middle_cursor = Cursor::new(Vec::new());
        {
            let mut middle_writer = ZipWriter::new(&mut middle_cursor);
            middle_writer
                .start_file("inner.zip", SimpleFileOptions::default())
                .expect("应创建内层嵌套归档条目");
            middle_writer
                .write_all(inner_cursor.get_ref())
                .expect("应写入内层嵌套归档");
            middle_writer.finish().expect("应完成中层归档");
        }
        let outer_path = directory.path().join("outer.zip");
        let mut outer_writer =
            ZipWriter::new(fs::File::create(&outer_path).expect("应创建外层归档"));
        outer_writer
            .start_file("middle.zip", SimpleFileOptions::default())
            .expect("应创建外层嵌套归档条目");
        outer_writer
            .write_all(middle_cursor.get_ref())
            .expect("应写入外层嵌套归档");
        outer_writer.finish().expect("应完成外层归档");

        // 深度上限为 1 时：middle.zip 展开，inner.zip 超出深度降级。
        let limited = SourceTreeScanner::scan_paths(
            vec![outer_path.clone()],
            LoaderConfig {
                max_archive_depth: 1,
                ..LoaderConfig::default()
            },
            ArchivePasswordStore::default(),
            tokio_util::sync::CancellationToken::new(),
            None,
        )
        .expect("限制深度的完整扫描应成功");
        assert_eq!(
            tree_order_labels(&limited.registry),
            vec!["outer.zip", "middle.zip", "inner.zip"]
        );
        let inner_node = limited
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|source_id| limited.registry.node(*source_id))
            .find(|node| node.label == "inner.zip")
            .expect("超限嵌套归档节点应存在");
        assert!(matches!(inner_node.kind, SourceKind::Unsupported(_)));
        assert!(inner_node.metadata.children_loaded);
        assert!(!inner_node.metadata.archive_password_required);

        // 默认深度上限为 2 时：inner.zip 展开且因单文件折叠为日志叶子。
        let full = SourceTreeScanner::scan_paths(
            vec![outer_path.clone()],
            LoaderConfig::default(),
            ArchivePasswordStore::default(),
            tokio_util::sync::CancellationToken::new(),
            None,
        )
        .expect("默认深度的完整扫描应成功");
        assert_eq!(
            tree_order_labels(&full.registry),
            vec!["outer.zip", "middle.zip", "inner.zip"]
        );
        let folded_node = full
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|source_id| full.registry.node(*source_id))
            .find(|node| node.label == "inner.zip")
            .expect("内层嵌套归档节点应存在");
        assert!(matches!(
            folded_node.kind,
            SourceKind::SingleFileArchive(ArchiveFormat::Zip)
        ));
        assert!(matches!(
            &folded_node.location,
            SourceLocation::ArchiveEntry {
                container_entries,
                entry_path,
                archive_depth: 2,
                ..
            } if container_entries == &["middle.zip".to_string(), "inner.zip".to_string()]
                && entry_path == "deep.log"
        ));
    }

    /// 验证加密压缩包在完整加载中降级为需要密码的提示节点，不中断整体扫描，且可凭密码仅重试该子树。
    #[test]
    fn scan_paths_marks_encrypted_archive_password_required_without_stopping() {
        let directory = temporary_test_dir("scan-paths-encrypted");
        fs::write(directory.path().join("normal.log"), "ready").expect("应写入普通日志");
        let secret_path = directory.path().join("secret.zip");
        write_encrypted_zip(&secret_path, "s3cret", &[("hidden.log", b"hidden")]);

        let result = SourceTreeScanner::scan_paths(
            vec![directory.path().to_path_buf()],
            LoaderConfig::default(),
            ArchivePasswordStore::default(),
            tokio_util::sync::CancellationToken::new(),
            None,
        )
        .expect("含加密压缩包的扫描应整体成功");

        // 普通日志仍被发现，加密压缩包降级为需要密码节点。
        assert!(tree_order_labels(&result.registry).contains(&"normal.log"));
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("secret.zip"))
        );
        let secret_node = result
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|source_id| result.registry.node(*source_id))
            .find(|node| node.label == "secret.zip")
            .expect("加密压缩包节点应存在");
        assert!(matches!(
            secret_node.kind,
            SourceKind::Archive(ArchiveFormat::Zip)
        ));
        assert!(!secret_node.metadata.children_loaded);
        assert!(secret_node.metadata.archive_password_required);
        assert_eq!(
            secret_node.metadata.message.as_deref(),
            Some("需要密码访问")
        );

        // 模拟点击重试：写入正确密码后仅重扫该子树，单文件归档折叠为日志叶子。
        let mut passwords = ArchivePasswordStore::default();
        passwords.insert(
            ArchivePasswordKey::root(secret_path.clone()),
            "s3cret".to_string(),
        );
        let subtree = SourceTreeScanner::scan_archive_subtree(
            secret_node,
            LoaderConfig::default(),
            passwords,
            tokio_util::sync::CancellationToken::new(),
        )
        .expect("写入正确密码后重试加密子树应成功");
        assert_eq!(tree_order_labels(&subtree.registry), vec!["secret.zip"]);
        let subtree_root = subtree
            .registry
            .node(subtree.registry.root_ids()[0])
            .expect("子树根应存在");
        assert!(matches!(
            subtree_root.kind,
            SourceKind::SingleFileArchive(ArchiveFormat::Zip)
        ));
        assert!(!subtree_root.metadata.archive_password_required);
    }

    /// 验证按节点重试子树：无密码或密码错误直接上抛 Err，写入正确密码后成功返回完整子树。
    #[test]
    fn scan_archive_subtree_surfaces_password_errors_and_succeeds_after_unlock() {
        let directory = temporary_test_dir("scan-archive-subtree");
        let secret_path = directory.path().join("secret.zip");
        write_encrypted_zip(
            &secret_path,
            "s3cret",
            &[("hidden/a.log", b"a"), ("hidden/b.log", b"b")],
        );
        let node = SourceTreeNode {
            id: SourceId(7),
            parent_id: None,
            depth: 0,
            label: "secret.zip".to_string(),
            kind: SourceKind::Archive(ArchiveFormat::Zip),
            location: SourceLocation::LocalPath(secret_path.clone()),
            metadata: SourceMetadata {
                archive_password_required: true,
                message: Some("需要密码访问".to_string()),
                ..SourceMetadata::default()
            },
            selected: false,
            expanded: false,
        };

        // 无密码：必须上抛缺少密码错误，不得降级为警告节点。
        let error = SourceTreeScanner::scan_archive_subtree(
            &node,
            LoaderConfig::default(),
            ArchivePasswordStore::default(),
            tokio_util::sync::CancellationToken::new(),
        )
        .expect_err("缺少密码时必须直接失败");
        assert_eq!(
            find_archive_password_error(&error).map(|error| error.kind),
            Some(ArchivePasswordErrorKind::Required)
        );

        // 错误密码：上抛密码无效错误，供界面再次弹窗。
        let mut wrong_passwords = ArchivePasswordStore::default();
        wrong_passwords.insert(
            ArchivePasswordKey::root(secret_path.clone()),
            "wrong".to_string(),
        );
        let error = SourceTreeScanner::scan_archive_subtree(
            &node,
            LoaderConfig::default(),
            wrong_passwords,
            tokio_util::sync::CancellationToken::new(),
        )
        .expect_err("密码错误时必须直接失败");
        assert_eq!(
            find_archive_password_error(&error).map(|error| error.kind),
            Some(ArchivePasswordErrorKind::Invalid)
        );

        // 正确密码：成功返回以该节点为根的完整子树。
        let mut passwords = ArchivePasswordStore::default();
        passwords.insert(
            ArchivePasswordKey::root(secret_path.clone()),
            "s3cret".to_string(),
        );
        let result = SourceTreeScanner::scan_archive_subtree(
            &node,
            LoaderConfig::default(),
            passwords,
            tokio_util::sync::CancellationToken::new(),
        )
        .expect("写入正确密码后子树重扫应成功");

        assert_eq!(
            tree_order_labels(&result.registry),
            vec!["secret.zip", "hidden", "a.log", "b.log"]
        );
        let root = result
            .registry
            .node(result.registry.root_ids()[0])
            .expect("子树根应存在");
        assert!(matches!(root.kind, SourceKind::Archive(ArchiveFormat::Zip)));
        assert!(root.metadata.children_loaded);
        assert!(!root.metadata.archive_password_required);
    }

    /// 验证嵌套压缩包节点的子树重试沿容器链路逐层读取，成功后子条目携带完整容器链路。
    #[test]
    fn scan_archive_subtree_reads_nested_container_chain() {
        let directory = temporary_test_dir("scan-archive-subtree-nested");
        let mut inner_cursor = Cursor::new(Vec::new());
        {
            let mut inner_writer = ZipWriter::new(&mut inner_cursor);
            for (name, content) in [("x.log", &b"x"[..]), ("y.log", &b"y"[..])] {
                inner_writer
                    .start_file(
                        name,
                        SimpleFileOptions::default()
                            .with_aes_encryption(zip::AesMode::Aes256, "pw"),
                    )
                    .expect("应创建加密内层条目");
                inner_writer.write_all(content).expect("应写入加密内层条目");
            }
            inner_writer.finish().expect("应完成加密内层归档");
        }
        let outer_path = directory.path().join("outer.zip");
        let mut outer_writer =
            ZipWriter::new(fs::File::create(&outer_path).expect("应创建外层归档"));
        outer_writer
            .start_file("inner.zip", SimpleFileOptions::default())
            .expect("应创建嵌套归档条目");
        outer_writer
            .write_all(inner_cursor.get_ref())
            .expect("应写入嵌套归档");
        outer_writer.finish().expect("应完成外层归档");

        let node = SourceTreeNode {
            id: SourceId(9),
            parent_id: None,
            depth: 0,
            label: "inner.zip".to_string(),
            kind: SourceKind::Archive(ArchiveFormat::Zip),
            location: SourceLocation::ArchiveEntry {
                archive_path: outer_path.clone(),
                root_format: ArchiveFormat::Zip,
                container_entries: Vec::new(),
                entry_path: "inner.zip".to_string(),
                format: ArchiveFormat::Zip,
                archive_depth: 0,
            },
            metadata: SourceMetadata {
                archive_password_required: true,
                message: Some("需要密码访问".to_string()),
                ..SourceMetadata::default()
            },
            selected: false,
            expanded: false,
        };

        // 无密码：沿容器链路读到嵌套压缩包后枚举失败，缺少密码错误直接上抛。
        let error = SourceTreeScanner::scan_archive_subtree(
            &node,
            LoaderConfig::default(),
            ArchivePasswordStore::default(),
            tokio_util::sync::CancellationToken::new(),
        )
        .expect_err("嵌套压缩包缺少密码时必须直接失败");
        assert_eq!(
            find_archive_password_error(&error).map(|error| error.kind),
            Some(ArchivePasswordErrorKind::Required)
        );

        // 正确密码写入嵌套容器键后重试成功。
        let mut passwords = ArchivePasswordStore::default();
        passwords.insert(
            ArchivePasswordKey::new(outer_path.clone(), &["inner.zip".to_string()]),
            "pw".to_string(),
        );
        let result = SourceTreeScanner::scan_archive_subtree(
            &node,
            LoaderConfig::default(),
            passwords,
            tokio_util::sync::CancellationToken::new(),
        )
        .expect("写入嵌套容器密码后子树重扫应成功");

        assert_eq!(
            tree_order_labels(&result.registry),
            vec!["inner.zip", "x.log", "y.log"]
        );
        let child = result
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|source_id| result.registry.node(*source_id))
            .find(|node| node.label == "x.log")
            .expect("嵌套子条目应存在");
        assert!(matches!(
            &child.location,
            SourceLocation::ArchiveEntry {
                archive_path,
                container_entries,
                entry_path,
                archive_depth: 1,
                ..
            } if archive_path == &outer_path
                && container_entries == &["inner.zip".to_string()]
                && entry_path == "x.log"
        ));
    }

    /// 验证取消令牌生效时 scan_paths 在路径边界检查处中断并返回 Err。
    #[test]
    fn scan_paths_stops_when_cancelled() {
        let directory = temporary_test_dir("scan-paths-cancelled");
        fs::write(directory.path().join("app.log"), "ready").expect("应写入测试日志");
        let cancellation = tokio_util::sync::CancellationToken::new();
        cancellation.cancel();

        let error = SourceTreeScanner::scan_paths(
            vec![directory.path().to_path_buf()],
            LoaderConfig::default(),
            ArchivePasswordStore::default(),
            cancellation,
            None,
        )
        .expect_err("取消后的完整加载必须立即停止");

        assert!(error.to_string().contains("已取消"));
    }

    /// 验证完整加载在目录和归档边界通过进度通道单调上报已处理节点数，并携带当前处理位置。
    #[test]
    fn scan_paths_reports_progress_at_directory_and_archive_boundaries() {
        let directory = temporary_test_dir("scan-paths-progress");
        fs::create_dir(directory.path().join("subdir")).expect("应创建子目录");
        fs::write(directory.path().join("subdir/deep.log"), "deep").expect("应写入深层日志");
        fs::write(directory.path().join("top.log"), "top").expect("应写入顶层日志");

        let (progress_sender, progress_receiver) = std::sync::mpsc::channel();
        let result = SourceTreeScanner::scan_paths(
            vec![directory.path().to_path_buf()],
            LoaderConfig::default(),
            ArchivePasswordStore::default(),
            tokio_util::sync::CancellationToken::new(),
            Some(progress_sender),
        )
        .expect("带进度的完整扫描应成功");

        let total = result.registry.tree_order_source_ids().len();
        // 扫描器随结果返回后释放发送端，通道自然断开，可一次性收取全部进度。
        let snapshots = progress_receiver.try_iter().collect::<Vec<_>>();
        assert!(!snapshots.is_empty());
        assert!(
            snapshots
                .windows(2)
                .all(|pair| pair[0].scanned <= pair[1].scanned),
            "进度计数必须单调不减：{snapshots:?}"
        );
        assert_eq!(
            snapshots.last().map(|snapshot| snapshot.scanned),
            Some(total)
        );
        assert!(
            snapshots
                .iter()
                .all(|snapshot| !snapshot.current.is_empty()),
            "每次进度上报都必须携带当前处理位置：{snapshots:?}"
        );
        assert!(
            snapshots
                .iter()
                .any(|snapshot| snapshot.current.contains("subdir")),
            "进度应展示正在处理的子目录：{snapshots:?}"
        );
    }

    /// 在内存中构造一个普通 ZIP 的字节，供嵌套归档夹具使用。
    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut cursor);
            for (name, content) in entries {
                writer
                    .start_file(*name, SimpleFileOptions::default())
                    .expect("应创建测试条目");
                writer.write_all(content).expect("应写入测试条目");
            }
            writer.finish().expect("应完成测试归档");
        }
        cursor.into_inner()
    }

    /// 验证嵌套压缩包批量展开后节点仍按排序位置发射，单文件嵌套包折叠行为不变。
    #[test]
    fn scan_paths_keeps_sorted_positions_for_batch_expanded_nested_archives() {
        let directory = temporary_test_dir("scan-paths-nested-order");
        let outer_path = directory.path().join("outer.zip");
        {
            let mut writer =
                ZipWriter::new(fs::File::create(&outer_path).expect("应创建外层测试归档"));
            for (name, content) in [
                ("a.log", b"a".to_vec()),
                (
                    "b.zip",
                    zip_bytes(&[("b1.log", &b"b1"[..]), ("b2.log", &b"b2"[..])]),
                ),
                ("c.log", b"c".to_vec()),
                ("d.zip", zip_bytes(&[("d-only.log", &b"d"[..])])),
            ] {
                writer
                    .start_file(name, SimpleFileOptions::default())
                    .expect("应创建外层测试条目");
                writer.write_all(&content).expect("应写入外层测试条目");
            }
            writer.finish().expect("应完成外层测试归档");
        }

        let result = SourceTreeScanner::scan_paths(
            vec![outer_path.clone()],
            LoaderConfig::default(),
            ArchivePasswordStore::default(),
            tokio_util::sync::CancellationToken::new(),
            None,
        )
        .expect("批量展开嵌套归档的扫描应成功");

        // b.zip/d.zip 与平铺日志交错时仍按名称排序位置出现，d.zip 折叠为单文件叶子。
        assert_eq!(
            tree_order_labels(&result.registry),
            vec![
                "outer.zip",
                "a.log",
                "b.zip",
                "b1.log",
                "b2.log",
                "c.log",
                "d.zip"
            ]
        );
        let b1_node = result
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|source_id| result.registry.node(*source_id))
            .find(|node| node.label == "b1.log")
            .expect("嵌套子条目应存在");
        assert!(matches!(
            &b1_node.location,
            SourceLocation::ArchiveEntry {
                archive_path,
                container_entries,
                entry_path,
                archive_depth: 1,
                ..
            } if archive_path == &outer_path
                && container_entries == &["b.zip".to_string()]
                && entry_path == "b1.log"
        ));
        let d_node = result
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|source_id| result.registry.node(*source_id))
            .find(|node| node.label == "d.zip")
            .expect("单文件嵌套归档节点应存在");
        assert!(matches!(
            d_node.kind,
            SourceKind::SingleFileArchive(ArchiveFormat::Zip)
        ));
    }

    /// 验证批量访问因单个条目密码不一致中止后逐条回退：该条目降级为需要密码节点，其余兄弟正常展开。
    #[test]
    fn scan_paths_falls_back_to_chain_reads_when_group_visit_aborts() {
        let directory = temporary_test_dir("scan-paths-nested-fallback");
        let outer_path = directory.path().join("outer.zip");
        {
            let mut writer =
                ZipWriter::new(fs::File::create(&outer_path).expect("应创建外层测试归档"));
            // e1.zip 用根密码加密（枚举校验通过），e2.zip 用不同密码加密（批量访问在此中止）。
            writer
                .start_file(
                    "e1.zip",
                    SimpleFileOptions::default().with_aes_encryption(zip::AesMode::Aes256, "p1"),
                )
                .expect("应创建 e1 测试条目");
            writer
                .write_all(&zip_bytes(&[
                    ("e1.log", &b"e1"[..]),
                    ("e1b.log", &b"e1b"[..]),
                ]))
                .expect("应写入 e1 嵌套归档");
            writer
                .start_file(
                    "e2.zip",
                    SimpleFileOptions::default().with_aes_encryption(zip::AesMode::Aes256, "p2"),
                )
                .expect("应创建 e2 测试条目");
            writer
                .write_all(&zip_bytes(&[("e2.log", &b"e2"[..])]))
                .expect("应写入 e2 嵌套归档");
            writer
                .start_file("plain.zip", SimpleFileOptions::default())
                .expect("应创建 plain 测试条目");
            writer
                .write_all(&zip_bytes(&[
                    ("plain.log", &b"plain"[..]),
                    ("plain2.log", &b"plain2"[..]),
                ]))
                .expect("应写入 plain 嵌套归档");
            writer.finish().expect("应完成外层测试归档");
        }

        let mut passwords = ArchivePasswordStore::default();
        passwords.insert(
            ArchivePasswordKey::root(outer_path.clone()),
            "p1".to_string(),
        );
        let result = SourceTreeScanner::scan_paths(
            vec![outer_path.clone()],
            LoaderConfig::default(),
            passwords,
            tokio_util::sync::CancellationToken::new(),
            None,
        )
        .expect("单个嵌套包密码不一致不应中断整体扫描");

        // e1 正常展开，e2 回退后仍失败并降级为需要密码节点，plain 正常展开。
        assert_eq!(
            tree_order_labels(&result.registry),
            vec![
                "outer.zip",
                "e1.zip",
                "e1.log",
                "e1b.log",
                "e2.zip",
                "plain.zip",
                "plain.log",
                "plain2.log"
            ]
        );
        let e2_node = result
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|source_id| result.registry.node(*source_id))
            .find(|node| node.label == "e2.zip")
            .expect("e2 节点应存在");
        assert!(matches!(
            e2_node.kind,
            SourceKind::Archive(ArchiveFormat::Zip)
        ));
        assert!(!e2_node.metadata.children_loaded);
        assert!(e2_node.metadata.archive_password_required);
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("e2.zip"))
        );
    }

    /// 验证压缩 TAR 根容器单遍批量展开多个嵌套压缩包，子条目携带完整容器链路。
    #[test]
    fn scan_paths_batch_expands_nested_archives_inside_compressed_tar() {
        let directory = temporary_test_dir("scan-paths-targz-nested");
        let tar_path = directory.path().join("bundle.tar.gz");
        let encoder = flate2::write::GzEncoder::new(
            fs::File::create(&tar_path).expect("应创建压缩 TAR 测试文件"),
            flate2::Compression::default(),
        );
        let mut builder = tar::Builder::new(encoder);
        for (name, data) in [
            (
                "a.zip",
                zip_bytes(&[("a1.log", &b"a1"[..]), ("a2.log", &b"a2"[..])]),
            ),
            ("b.zip", zip_bytes(&[("b-only.log", &b"b"[..])])),
            ("plain.log", b"plain".to_vec()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, name, data.as_slice())
                .expect("应写入 TAR 测试条目");
        }
        let encoder = builder.into_inner().expect("应完成 TAR 写入");
        encoder.finish().expect("应完成 gzip 压缩");

        let result = SourceTreeScanner::scan_paths(
            vec![tar_path.clone()],
            LoaderConfig::default(),
            ArchivePasswordStore::default(),
            tokio_util::sync::CancellationToken::new(),
            None,
        )
        .expect("压缩 TAR 批量展开嵌套归档的扫描应成功");

        // a.zip 展开两个子条目，b.zip 折叠为单文件叶子，plain.log 平铺。
        assert_eq!(
            tree_order_labels(&result.registry),
            vec![
                "bundle.tar.gz",
                "a.zip",
                "a1.log",
                "a2.log",
                "b.zip",
                "plain.log"
            ]
        );
        let a1_node = result
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|source_id| result.registry.node(*source_id))
            .find(|node| node.label == "a1.log")
            .expect("压缩 TAR 内嵌套子条目应存在");
        assert!(matches!(
            &a1_node.location,
            SourceLocation::ArchiveEntry {
                archive_path,
                container_entries,
                entry_path,
                archive_depth: 1,
                ..
            } if archive_path == &tar_path
                && container_entries == &["a.zip".to_string()]
                && entry_path == "a1.log"
        ));
    }
}
