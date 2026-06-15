//! 文件职责：构建真实日志来源目录树。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：加载本地文件、目录和压缩包结构，生成供 UI 虚拟列表消费的扁平注册表。

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::UNIX_EPOCH;

use anyhow::{Context as _, Result};
use thiserror::Error;

use crate::config::LoaderConfig;
use crate::loader::archive::adapter::{
    ArchiveEntryInfo, ArchiveRootProbe, list_archive_entries, list_archive_entries_from_bytes,
    read_archive_entry_bytes,
};
use crate::loader::archive::detector::{
    ArchiveFormat, detect_archive_format, detect_archive_format_by_name,
};
use crate::loader::archive::registry::archive_registry;
use crate::loader::log_source::{
    SourceId, SourceKind, SourceLocation, SourceMetadata, SourceTreeNode,
};
use crate::loader::source_registry::SourceRegistry;
use crate::utils::path::{display_name, normalize_archive_entry_path};

/// 日志来源加载器，当前只加载来源结构，不读取日志正文。
#[derive(Clone, Debug)]
pub struct LogSourceLoader {
    /// 加载模块配置，限制目录和压缩包展开策略。
    config: LoaderConfig,
    /// 是否延迟执行单文件压缩包探测；目录展开的快速路径只读缓存，不新增耗时探测。
    defer_archive_probe: bool,
}

/// 加载报告，记录本次新增节点、跳过项和错误说明。
#[derive(Debug)]
pub struct LoadReport {
    /// 本次加载生成的临时注册表。
    pub registry: SourceRegistry,
    /// 新增节点数量。
    pub added_count: usize,
    /// 跳过节点数量。
    pub skipped_count: usize,
    /// 用户可理解的错误或警告文案。
    pub errors: Vec<String>,
}

/// 来源树压缩包节点探测请求，由 UI 后台队列按优先级提交。
#[derive(Clone, Debug)]
pub struct SourceArchiveProbeRequest {
    /// 需要被回填的真实来源节点 ID。
    pub source_id: SourceId,
    /// 节点快照；后台探测不能直接读取 UI 注册表。
    pub node: SourceTreeNode,
}

/// 来源树压缩包节点探测结果；`patch == None` 表示不是单文件压缩包。
#[derive(Clone, Debug)]
pub struct SourceArchiveProbeResult {
    /// 结果对应的真实来源节点 ID。
    pub source_id: SourceId,
    /// 可回填的节点补丁；无补丁时保持原可展开压缩包行为。
    pub patch: Option<SourceArchiveProbePatch>,
}

/// 单文件压缩包探测成功后需要替换到来源节点上的数据。
#[derive(Clone, Debug)]
pub struct SourceArchiveProbePatch {
    /// 折叠后的节点类型。
    pub kind: SourceKind,
    /// 折叠后的真实打开位置。
    pub location: SourceLocation,
    /// 折叠后的节点元信息。
    pub metadata: SourceMetadata,
}

/// 日志来源加载错误。
#[derive(Debug, Error)]
pub enum LoadError {
    /// 来源路径不可读或不存在。
    #[error("无法读取来源路径：{0}")]
    UnreadablePath(String),
    /// 指定节点不支持子级加载。
    #[error("来源节点不支持展开：{0}")]
    UnsupportedExpansion(String),
}

/// 本地目录直接子项的轻量快照，避免排序阶段反复访问文件系统。
#[derive(Debug)]
struct LocalEntrySnapshot {
    /// 子项真实路径。
    path: PathBuf,
    /// 子项展示名称，排序和建节点复用，避免比较器反复分配。
    label: String,
    /// 小写排序键。
    sort_key: String,
    /// 是否为目录。
    is_dir: bool,
    /// 是否为符号链接。
    is_symlink: bool,
}

/// 根层单文件压缩包折叠后的打开目标。
#[derive(Clone, Debug)]
struct SingleFileArchiveTarget {
    /// 最终需要读取的内部普通文件路径。
    entry_path: String,
    /// 内部普通文件大小；探测阶段不为缺失大小额外解压统计。
    size: Option<u64>,
}

/// 压缩包当前目录层的直接子项快照。
#[derive(Clone, Debug)]
struct ArchiveLayerChild {
    /// 子项在当前压缩包容器中的完整规范化路径。
    entry_path: String,
    /// 子项展示名称。
    label: String,
    /// 小写排序键。
    sort_key: String,
    /// 是否为目录。
    is_dir: bool,
    /// 直接文件条目的大小。
    size: Option<u64>,
}

/// 压缩包根文件指纹，用于探测缓存失效判断。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ArchiveRootFingerprint {
    /// 外层压缩包文件长度。
    size: u64,
    /// 外层压缩包修改时间；无法读取时使用 0，保证缓存仍可工作但更保守。
    modified_nanos: u128,
}

/// 单文件压缩包探测缓存键。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ArchiveProbeCacheKey {
    /// 最外层真实压缩包路径。
    archive_path: PathBuf,
    /// 最外层压缩包格式。
    root_format: ArchiveFormat,
    /// 当前被探测压缩包所在的外层容器链路。
    container_entries: Vec<String>,
    /// 当前被探测的压缩包条目；本地压缩包自身使用空字符串。
    entry_path: String,
    /// 当前被探测压缩包格式。
    format: ArchiveFormat,
    /// 外层真实文件指纹。
    root_fingerprint: Option<ArchiveRootFingerprint>,
    /// 当前压缩包条目大小；用于内嵌条目发生变化时辅助失效。
    entry_size: Option<u64>,
}

/// 本地目录层压缩包探测任务。
#[derive(Clone, Debug)]
struct LocalArchiveProbeJob {
    /// 被探测的本地压缩包路径。
    path: PathBuf,
    /// 被探测压缩包格式。
    format: ArchiveFormat,
    /// 本地压缩包文件大小。
    entry_size: Option<u64>,
}

/// 压缩包虚拟目录层内嵌压缩包探测任务。
#[derive(Clone, Debug)]
struct NestedArchiveProbeJob {
    /// 最外层真实压缩包路径。
    archive_path: PathBuf,
    /// 最外层压缩包格式。
    root_format: ArchiveFormat,
    /// 当前被探测压缩包所在的外层容器链路。
    container_entries: Vec<String>,
    /// 当前被探测的直接子压缩包条目路径。
    entry_path: String,
    /// 当前被探测压缩包格式。
    format: ArchiveFormat,
    /// 当前压缩包条目大小。
    entry_size: Option<u64>,
}

/// 全局单文件压缩包探测缓存；跨展开操作复用，避免反复解析同一批压缩包。
static ARCHIVE_SINGLE_FILE_PROBE_CACHE: OnceLock<
    Mutex<HashMap<ArchiveProbeCacheKey, Option<SingleFileArchiveTarget>>>,
> = OnceLock::new();

/// 单文件压缩包探测缓存上限，避免用户反复浏览超大目录时进程级缓存无限增长。
const ARCHIVE_SINGLE_FILE_PROBE_CACHE_LIMIT: usize = 16_384;

impl LogSourceLoader {
    /// 使用指定配置创建日志来源加载器。
    pub fn new(config: LoaderConfig) -> Self {
        Self {
            config,
            defer_archive_probe: false,
        }
    }

    /// 创建只使用缓存、不新增同步探测的加载器。
    ///
    /// 说明：UI 展开目录时使用该模式，保证目录列表先快速返回；后台探测队列再渐进修正节点。
    pub fn with_deferred_archive_probe(mut self) -> Self {
        self.defer_archive_probe = true;
        self
    }

    /// 批量探测来源树中的压缩包节点，调用方负责按批次和优先级提交请求。
    pub fn probe_archive_nodes(
        &self,
        requests: Vec<SourceArchiveProbeRequest>,
    ) -> Vec<SourceArchiveProbeResult> {
        let results = Arc::new(Mutex::new(Vec::with_capacity(requests.len())));

        self.run_bounded_probe_jobs(requests, {
            let results = Arc::clone(&results);
            move |request| {
                let patch = probe_source_archive_node(&request.node);
                results
                    .lock()
                    .expect("来源压缩包探测结果锁不应被污染")
                    .push(SourceArchiveProbeResult {
                        source_id: request.source_id,
                        patch,
                    });
            }
        });

        match Arc::try_unwrap(results) {
            Ok(results) => results
                .into_inner()
                .expect("来源压缩包探测结果锁不应被污染"),
            Err(results) => results
                .lock()
                .expect("来源压缩包探测结果锁不应被污染")
                .clone(),
        }
    }

    /// 加载多个本地来源路径。
    ///
    /// 参数说明：
    /// - `paths`：来自自定义来源选择器的文件、目录或压缩包路径。
    ///
    /// 返回值：包含临时来源注册表的加载报告。
    pub fn load_paths(&self, paths: Vec<PathBuf>) -> LoadReport {
        let mut registry = SourceRegistry::new();
        let mut report = LoadReport::empty();

        for path in paths {
            match self.add_local_path(&mut registry, None, path.as_path(), 0, true) {
                Ok(count) => report.added_count += count,
                Err(error) => {
                    report.skipped_count += 1;
                    report.errors.push(error.to_string());
                }
            }
        }

        registry.rebuild_all_indices();
        report.registry = registry;
        report
    }

    /// 懒加载指定节点的直接子级。
    ///
    /// 参数说明：
    /// - `parent`：当前 UI 中被展开的来源节点快照。
    ///
    /// 返回值：只包含父节点子级的临时注册表；调用方负责挂回真实父节点。
    pub fn load_children(&self, parent: &SourceTreeNode) -> LoadReport {
        let mut registry = SourceRegistry::new();
        let mut report = LoadReport::empty();

        let result = match (&parent.kind, &parent.location) {
            (SourceKind::Directory, SourceLocation::LocalPath(path)) => {
                self.add_directory_children(&mut registry, None, path, 0)
            }
            (SourceKind::Archive(format), SourceLocation::LocalPath(path))
                if format.is_supported() =>
            {
                self.add_archive_children(&mut registry, None, path, *format, 0, 0)
            }
            (
                SourceKind::Archive(nested_format),
                SourceLocation::ArchiveEntry {
                    archive_path,
                    root_format,
                    container_entries,
                    entry_path,
                    format: _,
                    archive_depth,
                },
            ) if nested_format.is_supported() => self.add_nested_archive_children(
                &mut registry,
                None,
                archive_path,
                *root_format,
                container_entries,
                entry_path,
                *nested_format,
                0,
                archive_depth + 1,
            ),
            (
                SourceKind::ArchiveDirectory,
                SourceLocation::ArchiveEntry {
                    archive_path,
                    root_format,
                    container_entries,
                    entry_path,
                    format,
                    archive_depth,
                },
            ) if format.is_supported() => self.add_archive_directory_children(
                &mut registry,
                None,
                archive_path,
                *root_format,
                container_entries,
                *format,
                entry_path,
                0,
                *archive_depth,
            ),
            _ => Err(LoadError::UnsupportedExpansion(parent.label.clone()).into()),
        };

        match result {
            Ok(count) => report.added_count += count,
            Err(error) => {
                report.skipped_count += 1;
                report.errors.push(error.to_string());
            }
        }

        registry.rebuild_all_indices();
        report.registry = registry;
        report
    }

    /// 添加本地路径节点，并在需要时加载目录第一层子级。
    fn add_local_path(
        &self,
        registry: &mut SourceRegistry,
        parent_id: Option<SourceId>,
        path: &Path,
        depth: usize,
        load_first_level: bool,
    ) -> Result<usize> {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| LoadError::UnreadablePath(path.display().to_string()).to_string())?;
        if metadata.file_type().is_symlink() && !self.config.follow_symlinks {
            return Ok(self.add_unsupported_node(
                registry,
                parent_id,
                depth,
                display_name(path),
                SourceLocation::LocalPath(path.to_path_buf()),
                "符号链接".to_string(),
                Some("已跳过符号链接，避免目录循环".to_string()),
            ));
        }

        if metadata.is_dir() {
            let id = self.add_node(
                registry,
                parent_id,
                depth,
                display_name(path),
                SourceKind::Directory,
                SourceLocation::LocalPath(path.to_path_buf()),
                SourceMetadata {
                    size: None,
                    children_loaded: false,
                    is_loading: false,
                    message: None,
                },
                load_first_level,
            );

            let mut count = 1;
            if load_first_level {
                let child_count =
                    self.add_directory_children(registry, Some(id), path, depth + 1)?;
                if let Some(node) = registry.node_mut(id) {
                    node.metadata.children_loaded = true;
                    node.expanded = true;
                }
                count += child_count;
            }
            return Ok(count);
        }

        let kind = match detect_archive_format(path) {
            Some(format) if format.is_supported() => {
                if let Some(target) = self.single_file_target_for_local_archive(path, format) {
                    let location = SourceLocation::ArchiveEntry {
                        archive_path: path.to_path_buf(),
                        root_format: format,
                        container_entries: Vec::new(),
                        entry_path: target.entry_path,
                        format,
                        archive_depth: 0,
                    };
                    self.add_node(
                        registry,
                        parent_id,
                        depth,
                        display_name(path),
                        SourceKind::SingleFileArchive(format),
                        location,
                        SourceMetadata {
                            size: target.size,
                            children_loaded: true,
                            is_loading: false,
                            message: None,
                        },
                        false,
                    );
                    return Ok(1);
                }
                SourceKind::Archive(format)
            }
            Some(format) => SourceKind::Unsupported(format.label().to_string()),
            None => SourceKind::LogFile,
        };
        let can_expand = kind.can_expand();
        self.add_node(
            registry,
            parent_id,
            depth,
            display_name(path),
            kind,
            SourceLocation::LocalPath(path.to_path_buf()),
            SourceMetadata {
                size: Some(metadata.len()),
                children_loaded: !can_expand,
                is_loading: false,
                message: None,
            },
            false,
        );
        Ok(1)
    }

    /// 添加本地目录的直接子级，避免一次性递归大目录。
    fn add_directory_children(
        &self,
        registry: &mut SourceRegistry,
        parent_id: Option<SourceId>,
        path: &Path,
        depth: usize,
    ) -> Result<usize> {
        let mut entries = Vec::new();
        for entry in
            fs::read_dir(path).with_context(|| format!("无法读取目录：{}", path.display()))?
        {
            let entry = entry.with_context(|| format!("无法读取目录项：{}", path.display()))?;
            let metadata = entry
                .metadata()
                .or_else(|_| fs::symlink_metadata(entry.path()));
            if let Ok(metadata) = metadata {
                let entry_path = entry.path();
                let label = display_name(&entry_path);
                let sort_key = label.to_ascii_lowercase();
                entries.push(LocalEntrySnapshot {
                    path: entry_path,
                    label,
                    sort_key,
                    is_dir: metadata.is_dir(),
                    is_symlink: metadata.file_type().is_symlink(),
                });
            }
        }

        entries.sort_by(|left, right| {
            let left_group = if left.is_dir { 0 } else { 1 };
            let right_group = if right.is_dir { 0 } else { 1 };
            left_group
                .cmp(&right_group)
                .then_with(|| left.sort_key.cmp(&right.sort_key))
        });

        if !self.defer_archive_probe {
            self.probe_local_archive_children(&entries);
        }

        let mut count = 0;
        for entry in entries {
            if entry.is_symlink && !self.config.follow_symlinks {
                count += self.add_unsupported_node(
                    registry,
                    parent_id,
                    depth,
                    entry.label,
                    SourceLocation::LocalPath(entry.path),
                    "符号链接".to_string(),
                    Some("已跳过符号链接，避免目录循环".to_string()),
                );
                continue;
            }
            count += self.add_local_path(registry, parent_id, &entry.path, depth, false)?;
        }

        Ok(count)
    }

    /// 批量探测本地目录当前层的直接压缩包文件，并把结果写入进程级缓存。
    fn probe_local_archive_children(&self, entries: &[LocalEntrySnapshot]) {
        let jobs = entries
            .iter()
            .filter(|entry| !entry.is_dir)
            .filter(|entry| !entry.is_symlink || self.config.follow_symlinks)
            .filter_map(|entry| {
                let format = detect_archive_format(&entry.path)?;
                if !format.is_supported() {
                    return None;
                }
                let entry_size = fs::metadata(&entry.path)
                    .ok()
                    .map(|metadata| metadata.len());
                Some(LocalArchiveProbeJob {
                    path: entry.path.clone(),
                    format,
                    entry_size,
                })
            })
            .collect::<Vec<_>>();

        self.run_bounded_probe_jobs(jobs, |job| {
            let key = local_archive_probe_cache_key(&job.path, job.format, job.entry_size);
            let _ = cached_archive_probe_target(key, || {
                probe_local_single_file_target(&job.path, job.format)
            });
        });
    }

    /// 枚举压缩包条目并构建虚拟目录树。
    fn add_archive_children(
        &self,
        registry: &mut SourceRegistry,
        parent_id: Option<SourceId>,
        archive_path: &Path,
        format: ArchiveFormat,
        depth: usize,
        archive_depth: usize,
    ) -> Result<usize> {
        let entries = list_archive_entries(archive_path, format)?;
        Ok(self.add_archive_directory_children_from_entries(
            registry,
            parent_id,
            archive_path,
            format,
            Vec::new(),
            format,
            "",
            depth,
            archive_depth,
            entries,
        ))
    }

    /// 枚举嵌套压缩包条目并构建虚拟目录树。
    fn add_nested_archive_children(
        &self,
        registry: &mut SourceRegistry,
        parent_id: Option<SourceId>,
        archive_path: &Path,
        root_format: ArchiveFormat,
        container_entries: &[String],
        nested_entry_path: &str,
        nested_format: ArchiveFormat,
        depth: usize,
        nested_archive_depth: usize,
    ) -> Result<usize> {
        if nested_archive_depth > self.config.max_archive_depth {
            return Err(LoadError::UnsupportedExpansion(format!(
                "嵌套压缩包深度超过 {}，暂不展开",
                self.config.max_archive_depth
            ))
            .into());
        }

        let nested_bytes = read_archive_entry_bytes(
            archive_path,
            root_format,
            container_entries,
            nested_entry_path,
        )?;
        let source_label = crate::utils::path::archive_virtual_path(
            archive_path,
            container_entries,
            nested_entry_path,
        );
        let entries = list_archive_entries_from_bytes(nested_bytes, nested_format, &source_label)?;
        let mut nested_container_entries = container_entries.to_vec();
        nested_container_entries.push(normalize_archive_entry_path(nested_entry_path));

        Ok(self.add_archive_directory_children_from_entries(
            registry,
            parent_id,
            archive_path,
            root_format,
            nested_container_entries,
            nested_format,
            "",
            depth,
            nested_archive_depth,
            entries,
        ))
    }

    /// 枚举压缩包虚拟目录的直接子级。
    fn add_archive_directory_children(
        &self,
        registry: &mut SourceRegistry,
        parent_id: Option<SourceId>,
        archive_path: &Path,
        root_format: ArchiveFormat,
        container_entries: &[String],
        format: ArchiveFormat,
        directory_prefix: &str,
        depth: usize,
        archive_depth: usize,
    ) -> Result<usize> {
        let entries = self.list_current_archive_container_entries(
            archive_path,
            root_format,
            container_entries,
            format,
        )?;
        Ok(self.add_archive_directory_children_from_entries(
            registry,
            parent_id,
            archive_path,
            root_format,
            container_entries.to_vec(),
            format,
            directory_prefix,
            depth,
            archive_depth,
            entries,
        ))
    }

    /// 枚举当前压缩包容器条目。
    fn list_current_archive_container_entries(
        &self,
        archive_path: &Path,
        root_format: ArchiveFormat,
        container_entries: &[String],
        format: ArchiveFormat,
    ) -> Result<Vec<ArchiveEntryInfo>> {
        if container_entries.is_empty() {
            return list_archive_entries(archive_path, format);
        }

        let Some((current_container, parent_containers)) = container_entries.split_last() else {
            return list_archive_entries(archive_path, format);
        };
        let bytes = read_archive_entry_bytes(
            archive_path,
            root_format,
            parent_containers,
            current_container,
        )?;
        let source_label = crate::utils::path::archive_virtual_path(
            archive_path,
            parent_containers,
            current_container,
        );
        list_archive_entries_from_bytes(bytes, format, &source_label)
    }

    /// 返回本地压缩包是否可以折叠为单文件叶子节点。
    ///
    /// 说明：这里故意吞掉枚举失败并返回 `None`，保留旧行为，让用户展开节点时再看到具体错误。
    fn single_file_target_for_local_archive(
        &self,
        archive_path: &Path,
        format: ArchiveFormat,
    ) -> Option<SingleFileArchiveTarget> {
        let entry_size = fs::metadata(archive_path)
            .ok()
            .map(|metadata| metadata.len());
        let key = local_archive_probe_cache_key(archive_path, format, entry_size);
        if let Some(cached) = cached_archive_probe_target_if_present(&key) {
            return cached;
        }
        if self.defer_archive_probe {
            return None;
        }
        cached_archive_probe_target(key, || probe_local_single_file_target(archive_path, format))
    }

    /// 将压缩包扁平条目转换成指定目录层的直接子节点。
    fn add_archive_directory_children_from_entries(
        &self,
        registry: &mut SourceRegistry,
        parent_id: Option<SourceId>,
        archive_path: &Path,
        root_format: ArchiveFormat,
        container_entries: Vec<String>,
        format: ArchiveFormat,
        directory_prefix: &str,
        depth: usize,
        archive_depth: usize,
        entries: Vec<ArchiveEntryInfo>,
    ) -> usize {
        let mut children = collect_archive_layer_children(directory_prefix, &entries);
        children.sort_by(|left, right| {
            let left_group = if left.is_dir { 0 } else { 1 };
            let right_group = if right.is_dir { 0 } else { 1 };
            left_group
                .cmp(&right_group)
                .then_with(|| left.sort_key.cmp(&right.sort_key))
        });

        let archive_probe_results = if self.defer_archive_probe {
            self.cached_archive_layer_probe_results(
                archive_path,
                root_format,
                &container_entries,
                archive_depth,
                &children,
            )
        } else {
            self.probe_archive_layer_children(
                archive_path,
                root_format,
                &container_entries,
                archive_depth,
                &children,
            )
        };
        let mut count = 0;

        for child in children {
            if child.is_dir {
                self.add_node(
                    registry,
                    parent_id,
                    depth,
                    child.label,
                    SourceKind::ArchiveDirectory,
                    SourceLocation::ArchiveEntry {
                        archive_path: archive_path.to_path_buf(),
                        root_format,
                        container_entries: container_entries.clone(),
                        entry_path: child.entry_path,
                        format,
                        archive_depth,
                    },
                    SourceMetadata {
                        size: None,
                        children_loaded: false,
                        is_loading: false,
                        message: None,
                    },
                    false,
                );
                count += 1;
                continue;
            }

            let (kind, location, metadata) = self.classify_current_layer_file(
                archive_path,
                root_format,
                &container_entries,
                format,
                archive_depth,
                &child,
                archive_probe_results.get(&child.entry_path).cloned(),
            );
            self.add_node(
                registry,
                parent_id,
                depth,
                child.label,
                kind,
                location,
                metadata,
                false,
            );
            count += 1;
        }

        count
    }

    /// 批量探测压缩包虚拟目录当前层中的直接压缩包文件。
    fn probe_archive_layer_children(
        &self,
        archive_path: &Path,
        root_format: ArchiveFormat,
        container_entries: &[String],
        archive_depth: usize,
        children: &[ArchiveLayerChild],
    ) -> HashMap<String, Option<SingleFileArchiveTarget>> {
        let jobs = children
            .iter()
            .filter(|child| !child.is_dir)
            .filter_map(|child| {
                let format = detect_archive_format_by_name(&child.label)?;
                if !format.is_supported() || archive_depth + 1 > self.config.max_archive_depth {
                    return None;
                }
                Some(NestedArchiveProbeJob {
                    archive_path: archive_path.to_path_buf(),
                    root_format,
                    container_entries: container_entries.to_vec(),
                    entry_path: child.entry_path.clone(),
                    format,
                    entry_size: child.size,
                })
            })
            .collect::<Vec<_>>();

        let results = Arc::new(Mutex::new(HashMap::new()));
        self.run_bounded_probe_jobs(jobs, {
            let results = Arc::clone(&results);
            move |job| {
                let key = nested_archive_probe_cache_key(
                    &job.archive_path,
                    job.root_format,
                    &job.container_entries,
                    &job.entry_path,
                    job.format,
                    job.entry_size,
                );
                let target = cached_archive_probe_target(key, || {
                    probe_nested_single_file_target(
                        &job.archive_path,
                        job.root_format,
                        &job.container_entries,
                        &job.entry_path,
                        job.format,
                    )
                });
                results
                    .lock()
                    .expect("内嵌压缩包探测结果锁不应被污染")
                    .insert(job.entry_path, target);
            }
        });

        match Arc::try_unwrap(results) {
            Ok(results) => results
                .into_inner()
                .expect("内嵌压缩包探测结果锁不应被污染"),
            Err(results) => results
                .lock()
                .expect("内嵌压缩包探测结果锁不应被污染")
                .clone(),
        }
    }

    /// 只读取压缩包虚拟目录当前层的探测缓存，不触发新的同步探测。
    fn cached_archive_layer_probe_results(
        &self,
        archive_path: &Path,
        root_format: ArchiveFormat,
        container_entries: &[String],
        archive_depth: usize,
        children: &[ArchiveLayerChild],
    ) -> HashMap<String, Option<SingleFileArchiveTarget>> {
        children
            .iter()
            .filter(|child| !child.is_dir)
            .filter_map(|child| {
                let format = detect_archive_format_by_name(&child.label)?;
                if !format.is_supported() || archive_depth + 1 > self.config.max_archive_depth {
                    return None;
                }
                let key = nested_archive_probe_cache_key(
                    archive_path,
                    root_format,
                    container_entries,
                    &child.entry_path,
                    format,
                    child.size,
                );
                cached_archive_probe_target_if_present(&key)
                    .map(|target| (child.entry_path.clone(), target))
            })
            .collect()
    }

    /// 对当前目录层中的直接文件条目分类，并只对当前条目做单文件压缩包检测。
    fn classify_current_layer_file(
        &self,
        archive_path: &Path,
        root_format: ArchiveFormat,
        container_entries: &[String],
        format: ArchiveFormat,
        archive_depth: usize,
        child: &ArchiveLayerChild,
        precomputed_probe: Option<Option<SingleFileArchiveTarget>>,
    ) -> (SourceKind, SourceLocation, SourceMetadata) {
        let location = SourceLocation::ArchiveEntry {
            archive_path: archive_path.to_path_buf(),
            root_format,
            container_entries: container_entries.to_vec(),
            entry_path: child.entry_path.clone(),
            format,
            archive_depth,
        };
        let Some(nested_format) = detect_archive_format_by_name(&child.label) else {
            return (
                SourceKind::ArchiveFile,
                location,
                SourceMetadata {
                    size: child.size,
                    children_loaded: true,
                    is_loading: false,
                    message: None,
                },
            );
        };

        if archive_depth + 1 > self.config.max_archive_depth {
            return (
                SourceKind::Unsupported(format!("{} 超出深度", nested_format.label())),
                location,
                SourceMetadata {
                    size: child.size,
                    children_loaded: true,
                    is_loading: false,
                    message: Some(format!(
                        "嵌套压缩包深度超过 {}，暂不展开",
                        self.config.max_archive_depth
                    )),
                },
            );
        }

        if !nested_format.is_supported() {
            return (
                SourceKind::Unsupported(nested_format.label().to_string()),
                location,
                SourceMetadata {
                    size: child.size,
                    children_loaded: true,
                    is_loading: false,
                    message: Some("该压缩格式当前不可展开".to_string()),
                },
            );
        }

        let target = match precomputed_probe {
            Some(target) => target,
            None => self.single_file_target_for_archive_entry(
                archive_path,
                root_format,
                container_entries,
                &child.entry_path,
                nested_format,
                child.size,
            ),
        };

        if let Some(target) = target {
            let mut nested_container_entries = container_entries.to_vec();
            nested_container_entries.push(child.entry_path.clone());
            let location = SourceLocation::ArchiveEntry {
                archive_path: archive_path.to_path_buf(),
                root_format,
                container_entries: nested_container_entries,
                entry_path: target.entry_path,
                format: nested_format,
                archive_depth: archive_depth + 1,
            };
            return (
                SourceKind::SingleFileArchive(nested_format),
                location,
                SourceMetadata {
                    size: target.size,
                    children_loaded: true,
                    is_loading: false,
                    message: None,
                },
            );
        }

        (
            SourceKind::Archive(nested_format),
            location,
            SourceMetadata {
                size: child.size,
                children_loaded: false,
                is_loading: false,
                message: None,
            },
        )
    }

    /// 返回当前目录层直接压缩包文件是否可以折叠为单文件叶子。
    fn single_file_target_for_archive_entry(
        &self,
        archive_path: &Path,
        root_format: ArchiveFormat,
        container_entries: &[String],
        entry_path: &str,
        nested_format: ArchiveFormat,
        entry_size: Option<u64>,
    ) -> Option<SingleFileArchiveTarget> {
        let key = nested_archive_probe_cache_key(
            archive_path,
            root_format,
            container_entries,
            entry_path,
            nested_format,
            entry_size,
        );
        if let Some(cached) = cached_archive_probe_target_if_present(&key) {
            return cached;
        }
        if self.defer_archive_probe {
            return None;
        }
        cached_archive_probe_target(key, || {
            probe_nested_single_file_target(
                archive_path,
                root_format,
                container_entries,
                entry_path,
                nested_format,
            )
        })
    }

    /// 使用固定并发数执行压缩包探测任务，避免大目录一次性创建过多线程或阻塞 UI。
    fn run_bounded_probe_jobs<T, F>(&self, jobs: Vec<T>, worker: F)
    where
        T: Send + 'static,
        F: Fn(T) + Sync,
    {
        if jobs.is_empty() {
            return;
        }

        let worker_count = self
            .config
            .archive_probe_concurrency
            .clamp(1, 16)
            .min(jobs.len());
        let queue = Arc::new(Mutex::new(VecDeque::from(jobs)));

        thread::scope(|scope| {
            let worker_ref = &worker;
            for _ in 0..worker_count {
                let queue = Arc::clone(&queue);
                scope.spawn(move || {
                    loop {
                        let job = queue
                            .lock()
                            .expect("压缩包探测任务队列锁不应被污染")
                            .pop_front();
                        match job {
                            Some(job) => worker_ref(job),
                            None => break,
                        }
                    }
                });
            }
        });
    }

    /// 添加普通来源节点并返回分配的 ID。
    fn add_node(
        &self,
        registry: &mut SourceRegistry,
        parent_id: Option<SourceId>,
        depth: usize,
        label: String,
        kind: SourceKind,
        location: SourceLocation,
        metadata: SourceMetadata,
        expanded: bool,
    ) -> SourceId {
        let id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id,
            parent_id,
            depth,
            label,
            kind,
            location,
            metadata,
            selected: false,
            expanded,
        });
        id
    }

    /// 添加不支持节点并返回新增数量。
    fn add_unsupported_node(
        &self,
        registry: &mut SourceRegistry,
        parent_id: Option<SourceId>,
        depth: usize,
        label: String,
        location: SourceLocation,
        reason: String,
        message: Option<String>,
    ) -> usize {
        self.add_node(
            registry,
            parent_id,
            depth,
            label,
            SourceKind::Unsupported(reason),
            location,
            SourceMetadata {
                size: None,
                children_loaded: true,
                is_loading: false,
                message,
            },
            false,
        );
        1
    }
}

/// 从压缩包扁平条目中收集指定目录层的直接子项。
fn collect_archive_layer_children(
    directory_prefix: &str,
    entries: &[ArchiveEntryInfo],
) -> Vec<ArchiveLayerChild> {
    let prefix = normalize_archive_entry_path(directory_prefix);
    let mut children_by_path: HashMap<String, ArchiveLayerChild> = HashMap::new();

    for entry in entries {
        let Some(child) = archive_layer_child_from_entry(&prefix, entry) else {
            continue;
        };

        children_by_path
            .entry(child.entry_path.clone())
            .and_modify(|existing| {
                if child.is_dir {
                    existing.is_dir = true;
                    existing.size = None;
                } else if !existing.is_dir && existing.size.is_none() {
                    existing.size = child.size;
                }
            })
            .or_insert(child);
    }

    children_by_path.into_values().collect()
}

/// 将单个压缩包条目映射为当前目录层的直接子项。
fn archive_layer_child_from_entry(
    directory_prefix: &str,
    entry: &ArchiveEntryInfo,
) -> Option<ArchiveLayerChild> {
    let entry_path = normalize_archive_entry_path(&entry.path);
    if entry_path.is_empty() {
        return None;
    }

    let remainder = if directory_prefix.is_empty() {
        entry_path.as_str()
    } else {
        if entry_path == directory_prefix {
            return None;
        }
        let prefix_with_separator = format!("{directory_prefix}/");
        entry_path.strip_prefix(&prefix_with_separator)?
    };

    let mut parts = remainder.split('/').filter(|part| !part.is_empty());
    let direct_name = parts.next()?;
    let has_nested_parts = parts.next().is_some();
    let is_dir = entry.is_dir || has_nested_parts;
    let child_path = if directory_prefix.is_empty() {
        direct_name.to_string()
    } else {
        format!("{directory_prefix}/{direct_name}")
    };

    Some(ArchiveLayerChild {
        entry_path: child_path,
        label: direct_name.to_string(),
        sort_key: direct_name.to_ascii_lowercase(),
        is_dir,
        size: if is_dir { None } else { entry.size },
    })
}

/// 返回单文件压缩包探测缓存。
fn archive_probe_cache()
-> &'static Mutex<HashMap<ArchiveProbeCacheKey, Option<SingleFileArchiveTarget>>> {
    ARCHIVE_SINGLE_FILE_PROBE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 读取或写入单文件压缩包探测缓存。
///
/// 参数说明：
/// - `key`：包含真实路径、容器链路和文件指纹的缓存键。
/// - `probe`：缓存未命中时执行的轻量探测函数。
///
/// 返回值：可折叠的内部日志目标；不可折叠或探测失败时返回 `None`。
fn cached_archive_probe_target(
    key: ArchiveProbeCacheKey,
    probe: impl FnOnce() -> Option<SingleFileArchiveTarget>,
) -> Option<SingleFileArchiveTarget> {
    if let Some(cached) = archive_probe_cache()
        .lock()
        .expect("单文件压缩包探测缓存锁不应被污染")
        .get(&key)
        .cloned()
    {
        return cached;
    }

    let target = probe();
    let mut cache = archive_probe_cache()
        .lock()
        .expect("单文件压缩包探测缓存锁不应被污染");
    if cache.len() >= ARCHIVE_SINGLE_FILE_PROBE_CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(key, target.clone());
    target
}

/// 只读取缓存中的单文件压缩包探测结果，不触发新探测。
fn cached_archive_probe_target_if_present(
    key: &ArchiveProbeCacheKey,
) -> Option<Option<SingleFileArchiveTarget>> {
    archive_probe_cache()
        .lock()
        .expect("单文件压缩包探测缓存锁不应被污染")
        .get(key)
        .cloned()
}

/// 构造本地压缩包探测缓存键。
fn local_archive_probe_cache_key(
    archive_path: &Path,
    format: ArchiveFormat,
    entry_size: Option<u64>,
) -> ArchiveProbeCacheKey {
    ArchiveProbeCacheKey {
        archive_path: archive_path.to_path_buf(),
        root_format: format,
        container_entries: Vec::new(),
        entry_path: String::new(),
        format,
        root_fingerprint: archive_root_fingerprint(archive_path),
        entry_size,
    }
}

/// 构造内嵌压缩包探测缓存键。
fn nested_archive_probe_cache_key(
    archive_path: &Path,
    root_format: ArchiveFormat,
    container_entries: &[String],
    entry_path: &str,
    format: ArchiveFormat,
    entry_size: Option<u64>,
) -> ArchiveProbeCacheKey {
    ArchiveProbeCacheKey {
        archive_path: archive_path.to_path_buf(),
        root_format,
        container_entries: container_entries.to_vec(),
        entry_path: normalize_archive_entry_path(entry_path),
        format,
        root_fingerprint: archive_root_fingerprint(archive_path),
        entry_size,
    }
}

/// 读取外层真实压缩包文件指纹，用于探测缓存失效。
fn archive_root_fingerprint(path: &Path) -> Option<ArchiveRootFingerprint> {
    let metadata = fs::metadata(path).ok()?;
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();

    Some(ArchiveRootFingerprint {
        size: metadata.len(),
        modified_nanos,
    })
}

/// 对本地压缩包执行根层单文件轻量探测。
fn probe_local_single_file_target(
    archive_path: &Path,
    format: ArchiveFormat,
) -> Option<SingleFileArchiveTarget> {
    let probe = archive_registry()
        .probe_single_file_root(format, archive_path)
        .ok()?;
    single_file_target_from_probe(probe)
}

/// 对来源树中的可展开压缩包节点执行一次单文件探测，并生成 UI 节点补丁。
fn probe_source_archive_node(node: &SourceTreeNode) -> Option<SourceArchiveProbePatch> {
    let SourceKind::Archive(format) = node.kind else {
        return None;
    };

    match &node.location {
        SourceLocation::LocalPath(path) => {
            let entry_size = fs::metadata(path)
                .ok()
                .map(|metadata| metadata.len())
                .or(node.metadata.size);
            let key = local_archive_probe_cache_key(path, format, entry_size);
            let target =
                cached_archive_probe_target(key, || probe_local_single_file_target(path, format))?;
            let location = SourceLocation::ArchiveEntry {
                archive_path: path.clone(),
                root_format: format,
                container_entries: Vec::new(),
                entry_path: target.entry_path,
                format,
                archive_depth: 0,
            };
            Some(SourceArchiveProbePatch {
                kind: SourceKind::SingleFileArchive(format),
                location,
                metadata: SourceMetadata {
                    size: target.size,
                    children_loaded: true,
                    is_loading: false,
                    message: None,
                },
            })
        }
        SourceLocation::ArchiveEntry {
            archive_path,
            root_format,
            container_entries,
            entry_path,
            archive_depth,
            ..
        } => {
            let key = nested_archive_probe_cache_key(
                archive_path,
                *root_format,
                container_entries,
                entry_path,
                format,
                node.metadata.size,
            );
            let target = cached_archive_probe_target(key, || {
                probe_nested_single_file_target(
                    archive_path,
                    *root_format,
                    container_entries,
                    entry_path,
                    format,
                )
            })?;
            let mut nested_container_entries = container_entries.clone();
            nested_container_entries.push(normalize_archive_entry_path(entry_path));
            let location = SourceLocation::ArchiveEntry {
                archive_path: archive_path.clone(),
                root_format: *root_format,
                container_entries: nested_container_entries,
                entry_path: target.entry_path,
                format,
                archive_depth: archive_depth + 1,
            };
            Some(SourceArchiveProbePatch {
                kind: SourceKind::SingleFileArchive(format),
                location,
                metadata: SourceMetadata {
                    size: target.size,
                    children_loaded: true,
                    is_loading: false,
                    message: None,
                },
            })
        }
    }
}

/// 对内嵌压缩包执行根层单文件轻量探测。
fn probe_nested_single_file_target(
    archive_path: &Path,
    root_format: ArchiveFormat,
    container_entries: &[String],
    entry_path: &str,
    format: ArchiveFormat,
) -> Option<SingleFileArchiveTarget> {
    let bytes =
        read_archive_entry_bytes(archive_path, root_format, container_entries, entry_path).ok()?;
    let source_label =
        crate::utils::path::archive_virtual_path(archive_path, container_entries, entry_path);
    let reader_len = bytes.len() as u64;
    let mut probe_reader = Cursor::new(bytes.as_slice());
    let probe = archive_registry()
        .probe_single_file_root_from_reader(format, &mut probe_reader, reader_len, &source_label)
        .ok()?;
    single_file_target_from_probe(probe)
}

/// 将统一探测结果转换为来源树可直接打开的内部日志目标。
fn single_file_target_from_probe(probe: ArchiveRootProbe) -> Option<SingleFileArchiveTarget> {
    let ArchiveRootProbe::SingleFile(entry) = probe else {
        return None;
    };

    Some(SingleFileArchiveTarget {
        entry_path: normalize_archive_entry_path(&entry.path),
        size: entry.size,
    })
}

impl LoadReport {
    /// 构造空加载报告。
    pub fn empty() -> Self {
        Self {
            registry: SourceRegistry::new(),
            added_count: 0,
            skipped_count: 0,
            errors: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Cursor, Write};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::config::LoaderConfig;
    use crate::loader::archive::ArchiveEntryInfo;
    use crate::loader::archive::detector::ArchiveFormat;
    use crate::loader::dir_tree::{LogSourceLoader, SourceArchiveProbeRequest};
    use crate::loader::log_source::SourceKind;
    use crate::loader::source_registry::SourceRegistry;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use rars::{ArchiveVersion, FeatureSet, rar15_40};
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    /// 创建隔离的临时目录；使用进程 ID 和测试名降低冲突概率。
    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("argus_{name}_{}", std::process::id()))
    }

    /// 构造内存 ZIP 字节，便于测试嵌套压缩包展开逻辑。
    fn build_zip(entries: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(cursor);
        let options = SimpleFileOptions::default();

        for (name, bytes) in entries {
            writer.start_file(name, options).expect("应能创建 ZIP 条目");
            writer.write_all(&bytes).expect("应能写入 ZIP 条目内容");
        }

        writer.finish().expect("应能完成 ZIP 写入").into_inner()
    }

    /// 构造内存 TAR 字节，便于测试不同压缩格式之间的嵌套展开。
    fn build_tar(entries: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut bytes);
            for (name, content) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, name, content.as_slice())
                    .expect("应能写入 TAR 条目");
            }
            builder.finish().expect("应能完成 TAR 写入");
        }
        bytes
    }

    /// 构造 GZIP 单文件压缩字节，便于验证压缩包内 `.gz` 能继续展开。
    fn build_gzip(content: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(content).expect("应能写入 GZIP 内容");
        encoder.finish().expect("应能完成 GZIP 写入")
    }

    /// 构造只包含存储模式文件块的 RAR4 字节；用于验证 RAR 内嵌压缩包读取链路。
    fn build_rar4_stored(entries: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
        let mut bytes = b"Rar!\x1A\x07\x00".to_vec();
        push_u16(&mut bytes, 0);
        bytes.push(0x73);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 7);

        for (name, content) in entries {
            let header_size = 32 + name.len();
            push_u16(&mut bytes, 0);
            bytes.push(0x74);
            push_u16(&mut bytes, 0);
            push_u16(&mut bytes, header_size as u16);
            push_u32(&mut bytes, content.len() as u32);
            push_u32(&mut bytes, content.len() as u32);
            bytes.push(3);
            push_u32(&mut bytes, 0);
            push_u32(&mut bytes, 0);
            bytes.push(20);
            bytes.push(0x30);
            push_u16(&mut bytes, name.len() as u16);
            push_u32(&mut bytes, 0x20);
            bytes.extend(name.as_bytes());
            bytes.extend(content);
        }

        push_u16(&mut bytes, 0);
        bytes.push(0x7B);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 7);
        bytes
    }

    /// 构造 RAR2.9 压缩条目字节，用于验证真实 RAR 解码库路径。
    fn build_rar29_compressed(entries: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
        let file_entries = entries
            .iter()
            .map(|(name, content)| rar15_40::FileEntry {
                name: name.as_bytes(),
                data: content.as_slice(),
                file_time: 0,
                file_attr: 0x20,
                host_os: 3,
                password: None,
                file_comment: None,
            })
            .collect::<Vec<_>>();
        let options = rar15_40::WriterOptions::new(ArchiveVersion::Rar29, FeatureSet::store_only())
            .with_compression_level(1);

        rar15_40::write_compressed_archive(&file_entries, options)
            .expect("应能构造压缩 RAR 测试归档")
    }

    /// 写入小端 u16，供测试归档构造器复用。
    fn push_u16(bytes: &mut Vec<u8>, value: u16) {
        bytes.extend(value.to_le_bytes());
    }

    /// 写入小端 u32，供测试归档构造器复用。
    fn push_u32(bytes: &mut Vec<u8>, value: u32) {
        bytes.extend(value.to_le_bytes());
    }

    /// 验证目录加载只加载第一层子项，子目录保持懒加载状态。
    #[test]
    fn loads_directory_first_level_only() {
        let root = temp_root("loads_directory_first_level_only");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("child")).expect("应能创建测试子目录");
        let mut app_log = fs::File::create(root.join("app.log")).expect("应能创建测试日志文件");
        writeln!(app_log, "INFO boot").expect("应能写入测试日志");
        let mut nested_log =
            fs::File::create(root.join("child").join("nested.log")).expect("应能创建嵌套日志");
        writeln!(nested_log, "INFO nested").expect("应能写入嵌套日志");

        let loader = LogSourceLoader::new(LoaderConfig::default());
        let report = loader.load_paths(vec![root.clone()]);

        assert!(report.errors.is_empty());
        assert_eq!(report.added_count, 3);
        assert_eq!(report.registry.visible_source_ids().len(), 3);

        let labels = report
            .registry
            .visible_source_ids()
            .iter()
            .filter_map(|id| report.registry.node(*id))
            .map(|node| node.label.as_str())
            .collect::<Vec<_>>();
        assert!(labels.contains(&"app.log"));
        assert!(labels.contains(&"child"));
        assert!(!labels.contains(&"nested.log"));

        let child_node = report
            .registry
            .visible_source_ids()
            .iter()
            .filter_map(|id| report.registry.node(*id))
            .find(|node| node.label == "child")
            .expect("应存在 child 目录节点");
        assert!(matches!(child_node.kind, SourceKind::Directory));
        assert!(!child_node.metadata.children_loaded);

        let _ = fs::remove_dir_all(&root);
    }

    /// 验证无法读取内层字节时，当前层 ZIP 条目仍保守渲染成可展开节点。
    #[test]
    fn nested_zip_entry_is_expandable_archive_node() {
        let loader = LogSourceLoader::new(LoaderConfig::default());
        let mut registry = SourceRegistry::new();
        let added_count = loader.add_archive_directory_children_from_entries(
            &mut registry,
            None,
            Path::new("outer.zip"),
            ArchiveFormat::Zip,
            Vec::new(),
            ArchiveFormat::Zip,
            "",
            0,
            0,
            vec![ArchiveEntryInfo {
                path: "inner.zip".to_string(),
                label: "inner.zip".to_string(),
                is_dir: false,
                size: Some(128),
            }],
        );
        registry.rebuild_all_indices();

        assert_eq!(added_count, 1);
        let node = registry
            .tree_order_source_ids()
            .iter()
            .find_map(|id| registry.node(*id))
            .expect("应生成嵌套压缩包条目");
        assert!(node.kind.can_expand());
        assert!(matches!(node.kind, SourceKind::Archive(ArchiveFormat::Zip)));
        assert!(!node.metadata.children_loaded);
    }

    /// 验证本地根层单普通文件 ZIP 会折叠为可直接打开的压缩包叶子。
    #[test]
    fn local_single_file_zip_becomes_openable_archive_leaf() {
        let root = temp_root("local_single_file_zip_becomes_openable_archive_leaf");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("应能创建测试目录");
        let archive_path = root.join("outer.zip");
        fs::write(
            &archive_path,
            build_zip(vec![("app.log", b"INFO single".to_vec())]),
        )
        .expect("应能写入 ZIP");

        let loader = LogSourceLoader::new(LoaderConfig::default());
        let report = loader.load_paths(vec![archive_path.clone()]);
        assert!(
            report.errors.is_empty(),
            "单文件 ZIP 不应产生错误：{:?}",
            report.errors
        );
        let node = report
            .registry
            .tree_order_source_ids()
            .iter()
            .find_map(|id| report.registry.node(*id))
            .expect("应生成 ZIP 节点");

        assert!(matches!(
            node.kind,
            SourceKind::SingleFileArchive(ArchiveFormat::Zip)
        ));
        assert!(!node.kind.can_expand());
        assert!(node.kind.is_log_candidate());
        assert_eq!(node.label, "outer.zip");
        assert_eq!(node.metadata.size, Some(b"INFO single".len() as u64));
        assert_eq!(
            node.location.display_path(),
            format!("{}!/app.log", archive_path.display())
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// 验证本地目录直接子级中的单文件 ZIP 在目录加载阶段就会折叠。
    #[test]
    fn local_directory_single_file_zip_child_becomes_openable_leaf() {
        let root = temp_root("local_directory_single_file_zip_child_becomes_openable_leaf");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("应能创建测试目录");
        let archive_path = root.join("single.zip");
        fs::write(
            &archive_path,
            build_zip(vec![("app.log", b"INFO child zip".to_vec())]),
        )
        .expect("应能写入目录内 ZIP");

        let loader = LogSourceLoader::new(LoaderConfig::default());
        let report = loader.load_paths(vec![root.clone()]);
        assert!(
            report.errors.is_empty(),
            "目录中的单文件 ZIP 不应产生错误：{:?}",
            report.errors
        );
        let node = report
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|id| report.registry.node(*id))
            .find(|node| node.label == "single.zip")
            .expect("应生成 single.zip 子节点");

        assert!(matches!(
            node.kind,
            SourceKind::SingleFileArchive(ArchiveFormat::Zip)
        ));
        assert_eq!(node.metadata.size, Some(b"INFO child zip".len() as u64));
        assert_eq!(
            node.location.display_path(),
            format!("{}!/app.log", archive_path.display())
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// 验证唯一文件位于子目录中时不折叠压缩包，仍保持可展开目录结构。
    #[test]
    fn single_file_under_archive_directory_stays_expandable() {
        let root = temp_root("single_file_under_archive_directory_stays_expandable");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("应能创建测试目录");
        let archive_path = root.join("outer.zip");
        fs::write(
            &archive_path,
            build_zip(vec![("logs/app.log", b"INFO nested dir".to_vec())]),
        )
        .expect("应能写入 ZIP");

        let loader = LogSourceLoader::new(LoaderConfig::default());
        let report = loader.load_paths(vec![archive_path]);
        let node = report
            .registry
            .tree_order_source_ids()
            .iter()
            .find_map(|id| report.registry.node(*id))
            .expect("应生成 ZIP 节点");

        assert!(matches!(node.kind, SourceKind::Archive(ArchiveFormat::Zip)));
        assert!(node.kind.can_expand());

        let _ = fs::remove_dir_all(&root);
    }

    /// 验证内层压缩包包含多个根层文件时仍保持可展开。
    #[test]
    fn nested_multi_file_zip_stays_expandable() {
        let root = temp_root("nested_multi_file_zip_stays_expandable");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("应能创建测试目录");
        let outer_path = root.join("outer.zip");
        let inner_zip = build_zip(vec![
            ("app.log", b"INFO app".to_vec()),
            ("error.log", b"ERROR app".to_vec()),
        ]);
        fs::write(&outer_path, build_zip(vec![("inner.zip", inner_zip)])).expect("应能写入 ZIP");

        let loader = LogSourceLoader::new(LoaderConfig::default());
        let root_report = loader.load_paths(vec![outer_path]);
        let outer_node = root_report
            .registry
            .tree_order_source_ids()
            .iter()
            .find_map(|id| root_report.registry.node(*id))
            .expect("应生成外层 ZIP 节点")
            .clone();
        let first_level_report = loader.load_children(&outer_node);
        let inner_node = first_level_report
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|id| first_level_report.registry.node(*id))
            .find(|node| node.label == "inner.zip")
            .expect("应生成内层 ZIP 节点");

        assert!(matches!(
            inner_node.kind,
            SourceKind::Archive(ArchiveFormat::Zip)
        ));
        assert!(inner_node.kind.can_expand());

        let _ = fs::remove_dir_all(&root);
    }

    /// 验证内层 ZIP 的唯一日志位于子目录时，当前层不会误折叠该内层 ZIP。
    #[test]
    fn nested_zip_with_only_directory_child_stays_expandable() {
        let root = temp_root("nested_zip_with_only_directory_child_stays_expandable");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("应能创建测试目录");
        let outer_path = root.join("outer.zip");
        let inner_zip = build_zip(vec![("logs/app.log", b"INFO nested dir".to_vec())]);
        fs::write(&outer_path, build_zip(vec![("inner.zip", inner_zip)])).expect("应能写入 ZIP");

        let loader = LogSourceLoader::new(LoaderConfig::default());
        let root_report = loader.load_paths(vec![outer_path.clone()]);
        let outer_node = root_report
            .registry
            .tree_order_source_ids()
            .iter()
            .find_map(|id| root_report.registry.node(*id))
            .expect("应生成外层 ZIP 节点")
            .clone();
        let first_level_report = loader.load_children(&outer_node);
        let inner_node = first_level_report
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|id| first_level_report.registry.node(*id))
            .find(|node| node.label == "inner.zip")
            .expect("应生成内层 ZIP 节点");

        assert!(matches!(
            inner_node.kind,
            SourceKind::Archive(ArchiveFormat::Zip)
        ));
        assert!(inner_node.kind.can_expand());
        assert_eq!(
            inner_node.location.display_path(),
            format!("{}!/inner.zip", outer_path.display())
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// 验证外层 ZIP 展开时会把当前层单文件内嵌 ZIP 直接折叠成可打开叶子。
    #[test]
    fn expands_zip_entry_inside_zip_archive() {
        let root = temp_root("expands_zip_entry_inside_zip_archive");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("应能创建测试目录");
        let outer_path = root.join("outer.zip");
        let inner_zip = build_zip(vec![("nested.log", b"INFO nested".to_vec())]);
        let outer_zip = build_zip(vec![("inner.zip", inner_zip)]);
        fs::write(&outer_path, outer_zip).expect("应能写入外层 ZIP");

        let loader = LogSourceLoader::new(LoaderConfig::default());
        let root_report = loader.load_paths(vec![outer_path.clone()]);
        let outer_node = root_report
            .registry
            .tree_order_source_ids()
            .iter()
            .find_map(|id| root_report.registry.node(*id))
            .expect("应生成外层 ZIP 节点")
            .clone();
        let first_level_report = loader.load_children(&outer_node);
        assert!(
            first_level_report.errors.is_empty(),
            "外层 ZIP 第一层应能加载：{:?}",
            first_level_report.errors
        );
        let inner_node = first_level_report
            .registry
            .tree_order_source_ids()
            .iter()
            .find_map(|id| first_level_report.registry.node(*id))
            .expect("应生成内层 ZIP 节点")
            .clone();
        assert!(matches!(
            inner_node.kind,
            SourceKind::SingleFileArchive(ArchiveFormat::Zip)
        ));
        assert!(!inner_node.kind.can_expand());
        assert!(inner_node.kind.is_log_candidate());
        assert_eq!(inner_node.metadata.size, Some(b"INFO nested".len() as u64));
        assert_eq!(
            inner_node.location.display_path(),
            format!("{}!/inner.zip!/nested.log", outer_path.display())
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// 验证 ZIP 内嵌 GZIP 时当前层会直接折叠成可打开叶子。
    #[test]
    fn expands_gzip_entry_inside_zip_archive() {
        let root = temp_root("expands_gzip_entry_inside_zip_archive");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("应能创建测试目录");
        let outer_path = root.join("outer.zip");
        let inner_gzip = build_gzip(b"INFO nested gzip");
        let outer_zip = build_zip(vec![("nested.log.gz", inner_gzip)]);
        fs::write(&outer_path, outer_zip).expect("应能写入外层 ZIP");

        let loader = LogSourceLoader::new(LoaderConfig::default());
        let root_report = loader.load_paths(vec![outer_path.clone()]);
        let outer_node = root_report
            .registry
            .tree_order_source_ids()
            .iter()
            .find_map(|id| root_report.registry.node(*id))
            .expect("应生成外层 ZIP 节点")
            .clone();
        let first_level_report = loader.load_children(&outer_node);
        assert!(
            first_level_report.errors.is_empty(),
            "外层 ZIP 第一层应能加载：{:?}",
            first_level_report.errors
        );
        let gzip_node = first_level_report
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|id| first_level_report.registry.node(*id))
            .find(|node| node.label == "nested.log.gz")
            .expect("应生成内层 GZIP 节点")
            .clone();
        assert!(matches!(
            gzip_node.kind,
            SourceKind::SingleFileArchive(ArchiveFormat::Gzip)
        ));
        assert!(!gzip_node.kind.can_expand());
        assert!(gzip_node.kind.is_log_candidate());
        assert_eq!(gzip_node.metadata.size, None);
        assert_eq!(
            gzip_node.location.display_path(),
            format!("{}!/nested.log.gz!/nested.log", outer_path.display())
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// 验证后台来源节点探测会写入缓存，后续延迟加载同一路径可立即折叠。
    #[test]
    fn source_archive_probe_populates_deferred_loader_cache() {
        let root = temp_root("source_archive_probe_populates_deferred_loader_cache");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("应能创建测试目录");
        let archive_path = root.join("single.zip");
        fs::write(
            &archive_path,
            build_zip(vec![("app.log", b"INFO cached probe".to_vec())]),
        )
        .expect("应能写入 ZIP");

        let deferred_loader =
            LogSourceLoader::new(LoaderConfig::default()).with_deferred_archive_probe();
        let deferred_report = deferred_loader.load_paths(vec![archive_path.clone()]);
        let archive_node = deferred_report
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|id| deferred_report.registry.node(*id))
            .find(|node| node.label == "single.zip")
            .expect("延迟加载应先生成普通可展开压缩包节点")
            .clone();
        assert!(matches!(
            archive_node.kind,
            SourceKind::Archive(ArchiveFormat::Zip)
        ));

        let probe_results = deferred_loader.probe_archive_nodes(vec![SourceArchiveProbeRequest {
            source_id: archive_node.id,
            node: archive_node,
        }]);
        assert!(
            probe_results
                .first()
                .and_then(|result| result.patch.as_ref())
                .is_some(),
            "后台探测应识别单文件 ZIP"
        );

        let cached_report = deferred_loader.load_paths(vec![archive_path.clone()]);
        let cached_node = cached_report
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|id| cached_report.registry.node(*id))
            .find(|node| node.label == "single.zip")
            .expect("缓存命中后应仍生成 single.zip 节点");
        assert!(matches!(
            cached_node.kind,
            SourceKind::SingleFileArchive(ArchiveFormat::Zip)
        ));
        assert_eq!(
            cached_node.location.display_path(),
            format!("{}!/app.log", archive_path.display())
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// 验证 ZIP 中使用 Windows 反斜杠路径时仍能按规范化路径展开内嵌 ZIP。
    #[test]
    fn expands_zip_entry_with_windows_style_internal_path() {
        let root = temp_root("expands_zip_entry_with_windows_style_internal_path");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("应能创建测试目录");
        let outer_path = root.join("outer.zip");
        let inner_zip = build_zip(vec![("nested.log", b"INFO nested".to_vec())]);
        let outer_zip = build_zip(vec![("logs\\inner.zip", inner_zip)]);
        fs::write(&outer_path, outer_zip).expect("应能写入外层 ZIP");

        let loader = LogSourceLoader::new(LoaderConfig::default());
        let root_report = loader.load_paths(vec![outer_path.clone()]);
        let outer_node = root_report
            .registry
            .tree_order_source_ids()
            .iter()
            .find_map(|id| root_report.registry.node(*id))
            .expect("应生成外层 ZIP 节点")
            .clone();
        let first_level_report = loader.load_children(&outer_node);
        let logs_node = first_level_report
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|id| first_level_report.registry.node(*id))
            .find(|node| node.label == "logs")
            .expect("外层根目录只应生成 logs 虚拟目录")
            .clone();

        assert!(matches!(logs_node.kind, SourceKind::ArchiveDirectory));
        assert_eq!(
            logs_node.location.display_path(),
            format!("{}!/logs", outer_path.display())
        );

        let logs_report = loader.load_children(&logs_node);
        let inner_node = logs_report
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|id| logs_report.registry.node(*id))
            .find(|node| node.label == "inner.zip")
            .expect("展开 logs 后应生成反斜杠路径下的内层 ZIP 节点")
            .clone();
        assert!(matches!(
            inner_node.kind,
            SourceKind::SingleFileArchive(ArchiveFormat::Zip)
        ));
        assert_eq!(
            inner_node.location.display_path(),
            format!("{}!/logs/inner.zip!/nested.log", outer_path.display())
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// 验证 RAR 内嵌 ZIP 可以从父压缩包读取字节并继续展开。
    #[test]
    fn expands_zip_entry_inside_rar_archive() {
        let root = temp_root("expands_zip_entry_inside_rar_archive");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("应能创建测试目录");
        let outer_path = root.join("outer.rar");
        let inner_zip = build_zip(vec![("nested.log", b"INFO nested".to_vec())]);
        let outer_rar = build_rar4_stored(vec![("inner.zip", inner_zip)]);
        fs::write(&outer_path, outer_rar).expect("应能写入外层 RAR");

        let loader = LogSourceLoader::new(LoaderConfig::default());
        let root_report = loader.load_paths(vec![outer_path.clone()]);
        let outer_node = root_report
            .registry
            .tree_order_source_ids()
            .iter()
            .find_map(|id| root_report.registry.node(*id))
            .expect("应生成外层 RAR 节点")
            .clone();
        assert!(matches!(
            outer_node.kind,
            SourceKind::Archive(ArchiveFormat::Rar)
        ));

        let first_level_report = loader.load_children(&outer_node);
        assert!(
            first_level_report.errors.is_empty(),
            "外层 RAR 第一层应能加载：{:?}",
            first_level_report.errors
        );
        let inner_node = first_level_report
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|id| first_level_report.registry.node(*id))
            .find(|node| node.label == "inner.zip")
            .expect("应生成内层 ZIP 节点")
            .clone();
        assert!(matches!(
            inner_node.kind,
            SourceKind::SingleFileArchive(ArchiveFormat::Zip)
        ));
        assert_eq!(
            inner_node.location.display_path(),
            format!("{}!/inner.zip!/nested.log", outer_path.display())
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// 验证单文件压缩包探测缓存能复用结果，避免同一目录层重复解析同一压缩包。
    #[test]
    fn archive_probe_cache_reuses_cached_target() {
        let key = super::ArchiveProbeCacheKey {
            archive_path: PathBuf::from("cache-reuse.zip"),
            root_format: ArchiveFormat::Zip,
            container_entries: Vec::new(),
            entry_path: String::new(),
            format: ArchiveFormat::Zip,
            root_fingerprint: None,
            entry_size: Some(42),
        };
        let calls = Arc::new(AtomicUsize::new(0));

        let first_calls = Arc::clone(&calls);
        let first = super::cached_archive_probe_target(key.clone(), || {
            first_calls.fetch_add(1, Ordering::SeqCst);
            Some(super::SingleFileArchiveTarget {
                entry_path: "app.log".to_string(),
                size: Some(42),
            })
        });
        let second_calls = Arc::clone(&calls);
        let second = super::cached_archive_probe_target(key, || {
            second_calls.fetch_add(1, Ordering::SeqCst);
            Some(super::SingleFileArchiveTarget {
                entry_path: "other.log".to_string(),
                size: Some(1),
            })
        });

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(first.expect("首次探测应返回目标").entry_path, "app.log");
        assert_eq!(
            second.expect("缓存命中应返回首次目标").entry_path,
            "app.log"
        );
    }

    /// 验证压缩 RAR 内嵌 ZIP 时也能通过纯 Rust 解码路径继续展开。
    #[test]
    fn expands_zip_entry_inside_compressed_rar_archive() {
        let root = temp_root("expands_zip_entry_inside_compressed_rar_archive");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("应能创建测试目录");
        let outer_path = root.join("outer.rar");
        let inner_zip = build_zip(vec![("nested.log", b"INFO nested".to_vec())]);
        let outer_rar = build_rar29_compressed(vec![("inner.zip", inner_zip)]);
        fs::write(&outer_path, outer_rar).expect("应能写入外层 RAR");

        let loader = LogSourceLoader::new(LoaderConfig::default());
        let root_report = loader.load_paths(vec![outer_path.clone()]);
        let outer_node = root_report
            .registry
            .tree_order_source_ids()
            .iter()
            .find_map(|id| root_report.registry.node(*id))
            .expect("应生成外层 RAR 节点")
            .clone();
        let first_level_report = loader.load_children(&outer_node);
        assert!(
            first_level_report.errors.is_empty(),
            "压缩 RAR 第一层应能加载：{:?}",
            first_level_report.errors
        );
        let inner_node = first_level_report
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|id| first_level_report.registry.node(*id))
            .find(|node| node.label == "inner.zip")
            .expect("应生成内层 ZIP 节点")
            .clone();

        assert!(matches!(
            inner_node.kind,
            SourceKind::SingleFileArchive(ArchiveFormat::Zip)
        ));
        assert_eq!(
            inner_node.location.display_path(),
            format!("{}!/inner.zip!/nested.log", outer_path.display())
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// 验证三层混合压缩包链路会始终使用最外层格式读取真实文件。
    #[test]
    fn expands_three_level_mixed_archive_chain() {
        let root = temp_root("expands_three_level_mixed_archive_chain");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("应能创建测试目录");
        let outer_path = root.join("outer.rar");
        let inner_zip = build_zip(vec![("nested.log", b"INFO nested".to_vec())]);
        let middle_zip = build_zip(vec![("inner.zip", inner_zip)]);
        let outer_rar = build_rar29_compressed(vec![("middle.zip", middle_zip)]);
        fs::write(&outer_path, outer_rar).expect("应能写入外层 RAR");

        let loader = LogSourceLoader::new(LoaderConfig::default());
        let root_report = loader.load_paths(vec![outer_path.clone()]);
        let outer_node = root_report
            .registry
            .tree_order_source_ids()
            .iter()
            .find_map(|id| root_report.registry.node(*id))
            .expect("应生成外层 RAR 节点")
            .clone();
        let middle_report = loader.load_children(&outer_node);
        assert!(
            middle_report.errors.is_empty(),
            "外层 RAR 应能展开中层 ZIP：{:?}",
            middle_report.errors
        );
        let middle_node = middle_report
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|id| middle_report.registry.node(*id))
            .find(|node| node.label == "middle.zip")
            .expect("应生成中层 ZIP 节点")
            .clone();
        assert!(matches!(
            middle_node.kind,
            SourceKind::Archive(ArchiveFormat::Zip)
        ));

        let inner_report = loader.load_children(&middle_node);
        assert!(
            inner_report.errors.is_empty(),
            "中层 ZIP 应能展开内层 ZIP：{:?}",
            inner_report.errors
        );
        let inner_node = inner_report
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|id| inner_report.registry.node(*id))
            .find(|node| node.label == "inner.zip")
            .expect("应生成内层 ZIP 节点")
            .clone();
        assert!(matches!(
            inner_node.kind,
            SourceKind::SingleFileArchive(ArchiveFormat::Zip)
        ));
        assert_eq!(
            inner_node.location.display_path(),
            format!(
                "{}!/middle.zip!/inner.zip!/nested.log",
                outer_path.display()
            )
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// 验证 ZIP 内嵌 TAR 也会生成可展开节点并继续枚举内部日志。
    #[test]
    fn expands_tar_entry_inside_zip_archive() {
        let root = temp_root("expands_tar_entry_inside_zip_archive");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("应能创建测试目录");
        let outer_path = root.join("outer.zip");
        let inner_tar = build_tar(vec![("nested.log", b"INFO nested".to_vec())]);
        let outer_zip = build_zip(vec![("inner.tar", inner_tar)]);
        fs::write(&outer_path, outer_zip).expect("应能写入外层 ZIP");

        let loader = LogSourceLoader::new(LoaderConfig::default());
        let root_report = loader.load_paths(vec![outer_path.clone()]);
        let outer_node = root_report
            .registry
            .tree_order_source_ids()
            .iter()
            .find_map(|id| root_report.registry.node(*id))
            .expect("应生成外层 ZIP 节点")
            .clone();
        let first_level_report = loader.load_children(&outer_node);
        let inner_node = first_level_report
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|id| first_level_report.registry.node(*id))
            .find(|node| node.label == "inner.tar")
            .expect("应生成内层 TAR 节点")
            .clone();
        assert!(matches!(
            inner_node.kind,
            SourceKind::SingleFileArchive(ArchiveFormat::Tar)
        ));
        assert_eq!(
            inner_node.location.display_path(),
            format!("{}!/inner.tar!/nested.log", outer_path.display())
        );

        let _ = fs::remove_dir_all(&root);
    }
}
