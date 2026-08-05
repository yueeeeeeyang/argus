//! 文件职责：实现完全独立于 UI 来源加载流程的 Agent 全量来源扫描器。
//! 创建日期：2026-07-17
//! 修改日期：2026-07-17
//! 作者：Argus 开发团队
//! 主要功能：一次遍历本地目录、一次枚举每个归档容器、递归展开嵌套归档，并在结束时批量构建来源注册表。

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};

use crate::config::LoaderConfig;
use crate::loader::archive::adapter::{ArchiveEntryInfo, stream_archive_entry_with_passwords};
use crate::loader::archive::detector::{
    ArchiveFormat, detect_archive_format, detect_archive_format_by_name,
};
use crate::loader::archive::{
    ArchivePasswordKey, ArchivePasswordStore, archive_registry, find_archive_password_error,
};
use crate::loader::{
    SourceId, SourceKind, SourceLocation, SourceMetadata, SourceRegistry, SourceTreeNode,
};
use crate::utils::path::{display_name, normalize_archive_entry_path};

/// Agent 来源扫描结果；注册表已经完成一次性索引构建，可直接生成会话快照或回填主窗口。
#[derive(Debug)]
pub(crate) struct AgentSourceScanResult {
    /// 完整扫描后的来源注册表；未在当前授权范围内的根保持原状。
    pub registry: SourceRegistry,
    /// 可容忍的目录、归档和符号链接读取警告。
    pub warnings: Vec<String>,
}

/// 只供 Agent 会话准备使用的来源扫描器，不调用 UI 展开或渐进探测逻辑。
pub(crate) struct AgentSourceScanner<'a> {
    /// 扫描前的来源树，只用于取得根位置、复用稳定 ID 和保留未选根。
    original_registry: &'a SourceRegistry,
    /// 目录、符号链接和嵌套归档深度配置。
    config: LoaderConfig,
    /// 当前进程已授权的归档密码快照。
    archive_passwords: ArchivePasswordStore,
    /// 用户主动停止时由所有目录和归档边界检查的取消令牌。
    cancellation: tokio_util::sync::CancellationToken,
    /// 已有来源身份到稳定 ID 的映射和新增 ID 分配状态。
    stable_ids: StableSourceIds,
    /// 已访问真实目录，跟随符号链接时用于阻止循环。
    visited_directories: HashSet<PathBuf>,
    /// 去重后的安全警告。
    warnings: BTreeSet<String>,
    /// 父节点先于子节点的最终节点序列；结束时只重建一次索引。
    ordered_nodes: Vec<SourceTreeNode>,
}

impl<'a> AgentSourceScanner<'a> {
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
            stable_ids: StableSourceIds::from_registry(original_registry),
            visited_directories: HashSet::new(),
            warnings: BTreeSet::new(),
            ordered_nodes: Vec::new(),
        }
    }

    /// 完整扫描指定根；其它根的已有子树保持不变，避免单根智能分析破坏主窗口其它来源。
    pub(crate) fn scan(mut self, selected_root_ids: &[SourceId]) -> Result<AgentSourceScanResult> {
        self.ensure_not_cancelled()?;
        let selected_roots = selected_root_ids.iter().copied().collect::<HashSet<_>>();
        if selected_roots.is_empty() {
            bail!("Agent 来源扫描至少需要一个来源根");
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

        Ok(AgentSourceScanResult {
            registry: SourceRegistry::from_ordered_nodes(self.ordered_nodes),
            warnings: self.warnings.into_iter().collect(),
        })
    }

    /// 根据根节点的真实位置重新发现其完整内容，并始终保留根 ID。
    fn scan_selected_root(&mut self, root_id: SourceId) -> Result<()> {
        let root = self
            .original_registry
            .node(root_id)
            .cloned()
            .ok_or_else(|| anyhow!("Agent 来源根不存在"))?;
        match &root.location {
            SourceLocation::LocalPath(path) => {
                self.scan_local_root(&root, path.clone())?;
            }
            SourceLocation::ArchiveEntry {
                archive_path,
                root_format,
                ..
            } if root.parent_id.is_none() => {
                // 单文件归档根的 UI 位置已经折叠到内部条目；Agent 必须从真实外层归档重新枚举。
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

    /// 扫描本地目录、普通文件或归档根。
    fn scan_local_root(&mut self, root: &SourceTreeNode, path: PathBuf) -> Result<()> {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                self.push_node(
                    None,
                    root.depth,
                    root.label.clone(),
                    SourceKind::Unsupported("来源不可读".to_string()),
                    SourceLocation::LocalPath(path),
                    SourceMetadata {
                        message: Some("Agent 无法读取该来源".to_string()),
                        ..SourceMetadata::default()
                    },
                    Some(root.id),
                    &[],
                );
                self.warnings.insert(format!(
                    "无法读取来源根“{}”：{}",
                    root.label,
                    safe_io_error(&error)
                ));
                return Ok(());
            }
        };

        if metadata.file_type().is_symlink() && !self.config.follow_symlinks {
            self.push_node(
                None,
                root.depth,
                root.label.clone(),
                SourceKind::Unsupported("符号链接".to_string()),
                SourceLocation::LocalPath(path),
                SourceMetadata {
                    children_loaded: true,
                    message: Some("已跳过符号链接，避免目录循环".to_string()),
                    ..SourceMetadata::default()
                },
                Some(root.id),
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
            let root_id = self.push_node(
                None,
                root.depth,
                root.label.clone(),
                SourceKind::Directory,
                SourceLocation::LocalPath(path.clone()),
                SourceMetadata {
                    children_loaded: true,
                    ..SourceMetadata::default()
                },
                Some(root.id),
                &[],
            );
            self.scan_local_directory(root_id, &path, root.depth.saturating_add(1))?;
            return Ok(());
        }

        if let Some(format) = detect_archive_format(&path) {
            if format.is_supported() {
                return self.scan_local_archive(
                    None,
                    root.depth,
                    root.label.clone(),
                    path,
                    format,
                    Some(root.id),
                );
            }
            self.push_node(
                None,
                root.depth,
                root.label.clone(),
                SourceKind::Unsupported(format.label().to_string()),
                SourceLocation::LocalPath(path),
                SourceMetadata {
                    size: Some(followed_metadata.len()),
                    children_loaded: true,
                    message: Some("该压缩格式当前不可展开".to_string()),
                    ..SourceMetadata::default()
                },
                Some(root.id),
                &[],
            );
            return Ok(());
        }

        self.push_node(
            None,
            root.depth,
            root.label.clone(),
            SourceKind::LogFile,
            SourceLocation::LocalPath(path),
            SourceMetadata {
                size: Some(followed_metadata.len()),
                children_loaded: true,
                ..SourceMetadata::default()
            },
            Some(root.id),
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
        let compressed_location = SourceLocation::LocalPath(archive_path.clone());
        let entries = match self.list_local_archive(&archive_path, format) {
            Ok(entries) => entries,
            Err(error) => {
                let reason = archive_scan_error(&error);
                self.warnings
                    .insert(format!("无法枚举归档“{label}”：{reason}"));
                self.push_node(
                    parent_id,
                    depth,
                    label,
                    SourceKind::Archive(format),
                    compressed_location,
                    SourceMetadata {
                        size: fs::metadata(&archive_path)
                            .ok()
                            .map(|metadata| metadata.len()),
                        children_loaded: false,
                        message: Some(reason),
                        ..SourceMetadata::default()
                    },
                    preferred_id,
                    &[],
                );
                return Ok(());
            }
        };
        self.ensure_not_cancelled()?;
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
        self.emit_archive_children(archive_id, depth.saturating_add(1), &tree, &context)
    }

    /// 把已经枚举的归档树递归转换为来源节点；普通虚拟目录不会重新打开归档。
    fn emit_archive_children(
        &mut self,
        parent_id: SourceId,
        depth: usize,
        tree: &ArchiveTreeNode,
        context: &ArchiveContainerContext,
    ) -> Result<()> {
        for child in tree.sorted_children() {
            self.ensure_not_cancelled()?;
            let location = SourceLocation::ArchiveEntry {
                archive_path: context.archive_path.clone(),
                root_format: context.root_format,
                container_entries: context.container_entries.clone(),
                entry_path: child.full_path.clone(),
                format: context.format,
                archive_depth: context.archive_depth,
            };
            if child.is_directory() {
                let directory_id = self.push_node(
                    Some(parent_id),
                    depth,
                    child.name.clone(),
                    SourceKind::ArchiveDirectory,
                    location,
                    SourceMetadata {
                        children_loaded: true,
                        ..SourceMetadata::default()
                    },
                    None,
                    &[],
                );
                self.emit_archive_children(directory_id, depth.saturating_add(1), child, context)?;
                continue;
            }

            let Some(nested_format) = detect_archive_format_by_name(&child.name) else {
                self.push_node(
                    Some(parent_id),
                    depth,
                    child.name.clone(),
                    SourceKind::ArchiveFile,
                    location,
                    SourceMetadata {
                        size: child.size,
                        children_loaded: true,
                        ..SourceMetadata::default()
                    },
                    None,
                    &[],
                );
                continue;
            };
            if !nested_format.is_supported() {
                self.push_node(
                    Some(parent_id),
                    depth,
                    child.name.clone(),
                    SourceKind::Unsupported(nested_format.label().to_string()),
                    location,
                    SourceMetadata {
                        size: child.size,
                        children_loaded: true,
                        message: Some("该嵌套压缩格式当前不可展开".to_string()),
                        ..SourceMetadata::default()
                    },
                    None,
                    &[],
                );
                continue;
            }
            let nested_depth = context.archive_depth.saturating_add(1);
            if nested_depth > self.config.max_archive_depth {
                self.push_node(
                    Some(parent_id),
                    depth,
                    child.name.clone(),
                    SourceKind::Unsupported(format!("{} 超出深度", nested_format.label())),
                    location,
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
                continue;
            }
            self.scan_nested_archive(parent_id, depth, child, context, nested_format, location)?;
        }
        Ok(())
    }

    /// 读取一个嵌套归档条目一次、枚举一次，并立即丢弃压缩字节缓冲区。
    fn scan_nested_archive(
        &mut self,
        parent_id: SourceId,
        depth: usize,
        child: &ArchiveTreeNode,
        context: &ArchiveContainerContext,
        nested_format: ArchiveFormat,
        compressed_location: SourceLocation,
    ) -> Result<()> {
        self.ensure_not_cancelled()?;
        let mut bytes = Vec::with_capacity(
            child
                .size
                .and_then(|size| usize::try_from(size).ok())
                .unwrap_or_default(),
        );
        let cancellation = self.cancellation.clone();
        let read_result = stream_archive_entry_with_passwords(
            &context.archive_path,
            context.root_format,
            &context.container_entries,
            &child.full_path,
            &self.archive_passwords,
            &mut |chunk| {
                if cancellation.is_cancelled() {
                    bail!("来源树完整扫描已取消");
                }
                bytes.extend_from_slice(chunk);
                Ok(())
            },
        );
        match read_result {
            Ok(()) => {}
            Err(error) => {
                if self.cancellation.is_cancelled() {
                    bail!("来源树完整扫描已取消");
                }
                let reason = archive_scan_error(&error);
                self.warnings
                    .insert(format!("无法读取嵌套归档“{}”：{reason}", child.name));
                self.push_node(
                    Some(parent_id),
                    depth,
                    child.name.clone(),
                    SourceKind::Archive(nested_format),
                    compressed_location,
                    SourceMetadata {
                        size: child.size,
                        children_loaded: false,
                        message: Some(reason),
                        ..SourceMetadata::default()
                    },
                    None,
                    &[],
                );
                return Ok(());
            }
        }
        self.ensure_not_cancelled()?;
        let mut nested_container_entries = context.container_entries.clone();
        nested_container_entries.push(child.full_path.clone());
        let nested_label = child.name.clone();
        let source_label = nested_container_entries.join("!/");
        let password_key =
            ArchivePasswordKey::new(context.archive_path.clone(), &nested_container_entries);
        let mut reader = Cursor::new(bytes);
        let reader_len = reader.get_ref().len() as u64;
        let entries = match archive_registry().list_entries_from_reader_with_password_context(
            nested_format,
            &mut reader,
            reader_len,
            &source_label,
            self.archive_passwords.get(&password_key),
            password_key,
        ) {
            Ok(entries) => entries,
            Err(error) => {
                let reason = archive_scan_error(&error);
                self.warnings
                    .insert(format!("无法枚举嵌套归档“{nested_label}”：{reason}"));
                self.push_node(
                    Some(parent_id),
                    depth,
                    nested_label,
                    SourceKind::Archive(nested_format),
                    compressed_location,
                    SourceMetadata {
                        size: child.size,
                        children_loaded: false,
                        message: Some(reason),
                        ..SourceMetadata::default()
                    },
                    None,
                    &[],
                );
                return Ok(());
            }
        };
        self.ensure_not_cancelled()?;
        let tree = ArchiveTreeNode::from_entries(entries);
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
                Some(parent_id),
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
            Some(parent_id),
            depth,
            nested_label,
            SourceKind::Archive(nested_format),
            compressed_location,
            SourceMetadata {
                size: child.size,
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
        self.emit_archive_children(archive_id, depth.saturating_add(1), &tree, &nested_context)
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

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::io::Write;

    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    use super::*;
    use crate::config::paths::temporary_test_dir;

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

        let result = AgentSourceScanner::new(
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

        let result = AgentSourceScanner::new(
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

        let result = AgentSourceScanner::new(
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

        let result = AgentSourceScanner::new(
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

        let error = AgentSourceScanner::new(
            &registry,
            LoaderConfig::default(),
            ArchivePasswordStore::default(),
            cancellation,
        )
        .scan(&[root_id])
        .expect_err("取消后的独立扫描必须立即停止");

        assert!(error.to_string().contains("已取消"));
    }
}
