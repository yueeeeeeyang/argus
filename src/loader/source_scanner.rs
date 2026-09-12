//! 文件职责：实现来源树全量扫描器，为 AI 会话准备和目录树完整初始化提供统一的树构建能力。
//! 创建日期：2026-07-17
//! 修改日期：2026-09-12
//! 作者：Argus 开发团队
//! 主要功能：一次遍历工作目录中的普通文件树、把残留压缩包标记为不支持节点，并在结束时批量构建来源注册表。

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};

use crate::config::LoaderConfig;
use crate::loader::archive::detector::detect_archive_format;
use crate::loader::{
    SourceId, SourceKind, SourceLocation, SourceMetadata, SourceRegistry, SourceTreeNode,
};
use crate::utils::path::{display_name, display_path};

/// 来源树扫描结果；注册表已经完成一次性索引构建，可直接生成会话快照或回填主窗口。
#[derive(Debug)]
pub(crate) struct SourceTreeScanResult {
    /// 完整扫描后的来源注册表；未在当前授权范围内的根保持原状。
    pub registry: SourceRegistry,
    /// 可容忍的目录和符号链接读取警告。
    pub warnings: Vec<String>,
}

/// 日志加载所处的阶段；物化阶段在扫描阶段之前执行。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum SourceLoadPhase {
    /// 正在把来源复制/解压到工作目录。
    Materializing,
    /// 正在扫描来源树（默认，兼容只关心扫描进度的旧调用方）。
    #[default]
    Scanning,
}

/// 来源树扫描进度；`current` 描述正在处理的目录或物化文件，供界面展示当前处理位置。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SourceTreeScanProgress {
    /// 当前所处加载阶段。
    pub phase: SourceLoadPhase,
    /// 已生成的来源节点数或已物化文件数，单调不减。
    pub scanned: usize,
    /// 正在处理的目录、物化文件或来源根展示文本；扫描准备阶段为空。
    pub current: String,
}

/// 来源树全量扫描器，一次构建完整来源树，不调用 UI 展开或渐进探测逻辑。
pub(crate) struct SourceTreeScanner<'a> {
    /// 扫描前的来源树，只用于取得根位置、复用稳定 ID 和保留未选根。
    original_registry: &'a SourceRegistry,
    /// 目录与符号链接行为配置。
    config: LoaderConfig,
    /// 用户主动停止时由所有目录边界检查的取消令牌。
    cancellation: tokio_util::sync::CancellationToken,
    /// 可选的进度上报通道；开始处理目录前后发送进度快照，仅完整加载入口使用。
    progress: Option<std::sync::mpsc::Sender<SourceTreeScanProgress>>,
    /// 正在处理的目录或来源根展示文本，随进度快照一起发送。
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
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self {
            original_registry,
            config,
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
    /// - `paths`：待加载的本地文件或目录路径；识别为压缩包的路径不再展开，生成不支持节点。
    /// - `config`：目录与符号链接行为配置。
    /// - `cancellation`：新加载请求到来时用于中断本次扫描的取消令牌。
    /// - `progress`：可选进度通道，在开始处理每个目录前后发送进度快照。
    ///
    /// 说明：整树替换场景没有既有注册表，内部以空注册表创建扫描器，稳定 ID 自然从 1 开始分配；
    /// 目录递归在一次调用内完成，工作目录中残留的压缩包统一标记为不支持节点。
    pub(crate) fn scan_paths(
        paths: Vec<PathBuf>,
        config: LoaderConfig,
        cancellation: tokio_util::sync::CancellationToken,
        progress: Option<std::sync::mpsc::Sender<SourceTreeScanProgress>>,
    ) -> Result<SourceTreeScanResult> {
        let original_registry = SourceRegistry::new();
        let mut scanner = SourceTreeScanner::new(&original_registry, config, cancellation);
        scanner.progress = progress;
        scanner.scan_path_roots(paths)
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
        }
        Ok(())
    }

    /// 扫描本地目录或普通文件根；保持根 ID 稳定。
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
            );
            self.scan_local_directory(directory_id, &path, depth.saturating_add(1))?;
            return Ok(());
        }

        if let Some(format) = detect_archive_format(&path) {
            // 压缩包应在工作目录物化阶段展开为普通目录；扫到压缩包即未展开或不可展开，
            // 统一标记为不支持节点，扫描器不再枚举其内容。
            self.push_node(
                parent_id,
                depth,
                label,
                SourceKind::Unsupported(format.label().to_string()),
                SourceLocation::LocalPath(path),
                SourceMetadata {
                    size: Some(followed_metadata.len()),
                    children_loaded: true,
                    message: Some("压缩包未展开".to_string()),
                    ..SourceMetadata::default()
                },
                preferred_id,
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
        );
        Ok(())
    }

    /// 一次读取本地目录直接子项；子目录递归时不触发任何 UI 索引。
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
                );
                self.scan_local_directory(directory_id, &directory_path, depth.saturating_add(1))?;
                continue;
            }

            if let Some(format) = detect_archive_format(&entry.path) {
                // 与根路径同理：物化后残留的压缩包不再展开，直接标记为不支持节点。
                self.push_node(
                    Some(parent_id),
                    depth,
                    entry.label,
                    SourceKind::Unsupported(format.label().to_string()),
                    SourceLocation::LocalPath(entry.path),
                    SourceMetadata {
                        size: Some(entry.metadata.len()),
                        children_loaded: true,
                        message: Some("压缩包未展开".to_string()),
                        ..SourceMetadata::default()
                    },
                    None,
                );
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
            );
        }
        self.report_progress();
        Ok(())
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

    /// 插入一个节点并尽量复用相同来源路径的既有 ID、展开态和选中态。
    fn push_node(
        &mut self,
        parent_id: Option<SourceId>,
        depth: usize,
        label: String,
        kind: SourceKind,
        location: SourceLocation,
        metadata: SourceMetadata,
        preferred_id: Option<SourceId>,
    ) -> SourceId {
        let id = self.stable_ids.take(identity_path(&location), preferred_id);
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

    /// 在开始处理目录或来源根前更新当前处理项并立即上报，
    /// 保证大目录读取等长耗时操作期间界面能看到正在处理的位置。
    fn begin_progress_item(&mut self, current: String) {
        self.current_item = current;
        self.report_progress();
    }

    /// 上报当前进度快照；接收端随加载任务结束释放后，发送失败可安全忽略。
    fn report_progress(&self) {
        if let Some(progress) = &self.progress {
            let _ = progress.send(SourceTreeScanProgress {
                phase: SourceLoadPhase::Scanning,
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

/// 提取来源位置的稳定身份路径；物化后全部来源均为本地路径，直接以路径作为身份键。
fn identity_path(location: &SourceLocation) -> &Path {
    match location {
        SourceLocation::LocalPath(path) => path,
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
    /// 来源路径到既有 ID 队列。
    ids_by_path: HashMap<PathBuf, VecDeque<SourceId>>,
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
        let mut ids_by_path = HashMap::<PathBuf, VecDeque<SourceId>>::new();
        let mut state_by_id = HashMap::new();
        let mut next_id = 1_usize;
        for source_id in registry.tree_order_source_ids() {
            let Some(node) = registry.node(*source_id) else {
                continue;
            };
            ids_by_path
                .entry(identity_path(&node.location).to_path_buf())
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
            ids_by_path,
            state_by_id,
            used_ids: HashSet::new(),
            next_id,
        }
    }

    /// 提前保留未扫描子树 ID。
    fn reserve(&mut self, source_id: SourceId) {
        self.used_ids.insert(source_id);
    }

    /// 优先使用显式根 ID，其次按来源路径复用旧 ID，最后分配全新 ID。
    fn take(&mut self, path: &Path, preferred_id: Option<SourceId>) -> SourceId {
        if let Some(preferred_id) = preferred_id {
            self.used_ids.insert(preferred_id);
            return preferred_id;
        }
        if let Some(ids) = self.ids_by_path.get_mut(path) {
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

#[cfg(test)]
mod tests {
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

        let result = SourceTreeScanner::new(
            &registry,
            LoaderConfig::default(),
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

    /// 验证已取消扫描不会访问来源内容。
    #[test]
    fn cancelled_scan_stops_before_source_access() {
        let directory = temporary_test_dir("agent-native-source-cancelled");
        let (registry, root_id) = unloaded_directory_registry(directory.path());
        let cancellation = tokio_util::sync::CancellationToken::new();
        cancellation.cancel();

        let error = SourceTreeScanner::new(&registry, LoaderConfig::default(), cancellation)
            .scan(&[root_id])
            .expect_err("取消后的独立扫描必须立即停止");

        assert!(error.to_string().contains("已取消"));
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

    /// 验证 scan_paths 从混合路径列表一次性完整建树：目录递归与普通文件识别全部在加载时完成。
    #[test]
    fn scan_paths_builds_complete_tree_from_mixed_paths() {
        let directory = temporary_test_dir("scan-paths-mixed");
        let root_label = display_name(directory.path());
        fs::create_dir(directory.path().join("subdir")).expect("应创建子目录");
        fs::write(directory.path().join("subdir/deep.log"), "deep").expect("应写入深层日志");
        fs::write(directory.path().join("alpha.log"), "alpha").expect("应写入顶层日志");

        let extra = temporary_test_dir("scan-paths-extra");
        let standalone_path = extra.path().join("standalone.log");
        fs::write(&standalone_path, "solo").expect("应写入独立日志");

        let result = SourceTreeScanner::scan_paths(
            vec![directory.path().to_path_buf(), standalone_path],
            LoaderConfig::default(),
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
                "standalone.log"
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
    }

    /// 验证物化后工作目录中残留的压缩包不再展开，在来源树上标记为不支持节点。
    #[test]
    fn scan_paths_marks_leftover_archive_as_unsupported() {
        let directory = temporary_test_dir("scan-paths-leftover-archive");
        fs::write(directory.path().join("app.log"), "ready").expect("应写入测试日志");
        fs::write(directory.path().join("bundle.zip"), b"not-expanded").expect("应写入残留压缩包");

        let result = SourceTreeScanner::scan_paths(
            vec![directory.path().to_path_buf()],
            LoaderConfig::default(),
            tokio_util::sync::CancellationToken::new(),
            None,
        )
        .expect("含残留压缩包的完整扫描应成功");

        let nodes = result
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|source_id| result.registry.node(*source_id))
            .collect::<Vec<_>>();
        let log_node = nodes
            .iter()
            .find(|node| node.label == "app.log")
            .expect("普通日志节点应存在");
        assert!(matches!(log_node.kind, SourceKind::LogFile));
        let archive_node = nodes
            .iter()
            .find(|node| node.label == "bundle.zip")
            .expect("残留压缩包节点应存在");
        assert!(matches!(
            &archive_node.kind,
            SourceKind::Unsupported(format) if format == "ZIP"
        ));
        assert!(archive_node.metadata.children_loaded);
        assert_eq!(archive_node.metadata.size, Some(12));
        assert_eq!(
            archive_node.metadata.message.as_deref(),
            Some("压缩包未展开")
        );
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
            cancellation,
            None,
        )
        .expect_err("取消后的完整加载必须立即停止");

        assert!(error.to_string().contains("已取消"));
    }

    /// 验证完整加载在目录边界通过进度通道单调上报已处理节点数，并携带当前处理位置。
    #[test]
    fn scan_paths_reports_progress_at_directory_boundaries() {
        let directory = temporary_test_dir("scan-paths-progress");
        fs::create_dir(directory.path().join("subdir")).expect("应创建子目录");
        fs::write(directory.path().join("subdir/deep.log"), "deep").expect("应写入深层日志");
        fs::write(directory.path().join("top.log"), "top").expect("应写入顶层日志");

        let (progress_sender, progress_receiver) = std::sync::mpsc::channel();
        let result = SourceTreeScanner::scan_paths(
            vec![directory.path().to_path_buf()],
            LoaderConfig::default(),
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
}
