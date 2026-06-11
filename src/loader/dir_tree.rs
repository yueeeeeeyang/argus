//! 文件职责：构建真实日志来源目录树。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：加载本地文件、目录和压缩包结构，生成供 UI 虚拟列表消费的扁平注册表。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use thiserror::Error;

use crate::config::LoaderConfig;
use crate::loader::archive::adapter::{
    ArchiveEntryInfo, list_archive_entries, list_archive_entries_from_bytes,
    read_archive_entry_bytes,
};
use crate::loader::archive::detector::{
    ArchiveFormat, detect_archive_format, detect_archive_format_by_name,
};
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

impl LogSourceLoader {
    /// 使用指定配置创建日志来源加载器。
    pub fn new(config: LoaderConfig) -> Self {
        Self { config }
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
            Some(format) if format.is_supported() => SourceKind::Archive(format),
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
        Ok(self.add_archive_entries(
            registry,
            parent_id,
            archive_path,
            format,
            Vec::new(),
            format,
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

        Ok(self.add_archive_entries(
            registry,
            parent_id,
            archive_path,
            root_format,
            nested_container_entries,
            nested_format,
            depth,
            nested_archive_depth,
            entries,
        ))
    }

    /// 将压缩包扁平条目转换成来源树节点。
    fn add_archive_entries(
        &self,
        registry: &mut SourceRegistry,
        parent_id: Option<SourceId>,
        archive_path: &Path,
        root_format: ArchiveFormat,
        container_entries: Vec<String>,
        format: ArchiveFormat,
        depth: usize,
        archive_depth: usize,
        mut entries: Vec<ArchiveEntryInfo>,
    ) -> usize {
        entries.sort_by(|left, right| {
            let left_group = if left.is_dir { 0 } else { 1 };
            let right_group = if right.is_dir { 0 } else { 1 };
            left_group.cmp(&right_group).then_with(|| {
                left.path
                    .to_ascii_lowercase()
                    .cmp(&right.path.to_ascii_lowercase())
            })
        });

        let mut directory_ids: HashMap<String, SourceId> = HashMap::new();
        let mut count = 0;

        for entry in entries {
            let entry_path = normalize_archive_entry_path(&entry.path);
            if entry_path.is_empty() {
                continue;
            }

            let parts = entry_path
                .split('/')
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>();
            if parts.is_empty() {
                continue;
            }

            let mut current_parent = parent_id;
            let mut current_depth = depth;
            let mut prefix = String::new();

            for (part_index, part) in parts.iter().enumerate() {
                let is_last = part_index == parts.len() - 1;
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);

                if !is_last || entry.is_dir {
                    if let Some(existing_id) = directory_ids.get(&prefix).copied() {
                        current_parent = Some(existing_id);
                        current_depth += 1;
                        continue;
                    }

                    let id = self.add_node(
                        registry,
                        current_parent,
                        current_depth,
                        (*part).to_string(),
                        SourceKind::ArchiveDirectory,
                        SourceLocation::ArchiveEntry {
                            archive_path: archive_path.to_path_buf(),
                            root_format,
                            container_entries: container_entries.clone(),
                            entry_path: prefix.clone(),
                            format,
                            archive_depth,
                        },
                        SourceMetadata {
                            size: None,
                            children_loaded: true,
                            is_loading: false,
                            message: None,
                        },
                        false,
                    );
                    directory_ids.insert(prefix.clone(), id);
                    current_parent = Some(id);
                    current_depth += 1;
                    count += 1;
                    continue;
                }

                let nested_format = detect_archive_format_by_name(part);
                let (kind, message, children_loaded) = match nested_format {
                    Some(nested_format) if archive_depth + 1 > self.config.max_archive_depth => (
                        SourceKind::Unsupported(format!("{} 超出深度", nested_format.label())),
                        Some(format!(
                            "嵌套压缩包深度超过 {}，暂不展开",
                            self.config.max_archive_depth
                        )),
                        true,
                    ),
                    Some(nested_format) if nested_format.is_supported() => {
                        (SourceKind::Archive(nested_format), None, false)
                    }
                    Some(nested_format) => (
                        SourceKind::Unsupported(nested_format.label().to_string()),
                        Some("该压缩格式当前不可展开".to_string()),
                        true,
                    ),
                    None => (SourceKind::ArchiveFile, None, true),
                };

                self.add_node(
                    registry,
                    current_parent,
                    current_depth,
                    entry.label.clone(),
                    kind,
                    SourceLocation::ArchiveEntry {
                        archive_path: archive_path.to_path_buf(),
                        root_format,
                        container_entries: container_entries.clone(),
                        entry_path: entry_path.clone(),
                        format,
                        archive_depth,
                    },
                    SourceMetadata {
                        size: entry.size,
                        children_loaded,
                        is_loading: false,
                        message,
                    },
                    false,
                );
                count += 1;
            }
        }

        count
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

    use crate::config::LoaderConfig;
    use crate::loader::archive::ArchiveEntryInfo;
    use crate::loader::archive::detector::ArchiveFormat;
    use crate::loader::dir_tree::LogSourceLoader;
    use crate::loader::log_source::SourceKind;
    use crate::loader::source_registry::SourceRegistry;
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

    /// 验证 ZIP 压缩包内部的 ZIP 会被渲染成可展开节点。
    #[test]
    fn nested_zip_entry_is_expandable_archive_node() {
        let loader = LogSourceLoader::new(LoaderConfig::default());
        let mut registry = SourceRegistry::new();
        let added_count = loader.add_archive_entries(
            &mut registry,
            None,
            Path::new("outer.zip"),
            ArchiveFormat::Zip,
            Vec::new(),
            ArchiveFormat::Zip,
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

    /// 验证 ZIP 内嵌 ZIP 可以从父压缩包读取字节并枚举内部日志条目。
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
            SourceKind::Archive(ArchiveFormat::Zip)
        ));

        let nested_report = loader.load_children(&inner_node);
        assert!(
            nested_report.errors.is_empty(),
            "内层 ZIP 应能展开：{:?}",
            nested_report.errors
        );
        let labels = nested_report
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|id| nested_report.registry.node(*id))
            .map(|node| node.label.as_str())
            .collect::<Vec<_>>();

        assert_eq!(labels, vec!["nested.log"]);
        let nested_log = nested_report
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|id| nested_report.registry.node(*id))
            .find(|node| node.label == "nested.log")
            .expect("应生成内层日志条目");
        assert!(matches!(nested_log.kind, SourceKind::ArchiveFile));
        assert_eq!(
            nested_log.location.display_path(),
            format!("{}!/inner.zip!/nested.log", outer_path.display())
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
        let inner_node = first_level_report
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|id| first_level_report.registry.node(*id))
            .find(|node| node.label == "inner.zip")
            .expect("应生成反斜杠路径下的内层 ZIP 节点")
            .clone();

        let nested_report = loader.load_children(&inner_node);
        assert!(
            nested_report.errors.is_empty(),
            "反斜杠路径内层 ZIP 应能展开：{:?}",
            nested_report.errors
        );
        let labels = nested_report
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|id| nested_report.registry.node(*id))
            .map(|node| node.label.as_str())
            .collect::<Vec<_>>();

        assert_eq!(labels, vec!["nested.log"]);

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
            SourceKind::Archive(ArchiveFormat::Zip)
        ));

        let nested_report = loader.load_children(&inner_node);
        assert!(
            nested_report.errors.is_empty(),
            "RAR 内层 ZIP 应能展开：{:?}",
            nested_report.errors
        );
        let labels = nested_report
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|id| nested_report.registry.node(*id))
            .map(|node| node.label.as_str())
            .collect::<Vec<_>>();

        assert_eq!(labels, vec!["nested.log"]);

        let _ = fs::remove_dir_all(&root);
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

        let nested_report = loader.load_children(&inner_node);
        assert!(
            nested_report.errors.is_empty(),
            "压缩 RAR 内层 ZIP 应能展开：{:?}",
            nested_report.errors
        );
        let labels = nested_report
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|id| nested_report.registry.node(*id))
            .map(|node| node.label.as_str())
            .collect::<Vec<_>>();

        assert_eq!(labels, vec!["nested.log"]);

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

        let nested_report = loader.load_children(&inner_node);
        assert!(
            nested_report.errors.is_empty(),
            "内层 ZIP 应能展开日志：{:?}",
            nested_report.errors
        );
        let labels = nested_report
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|id| nested_report.registry.node(*id))
            .map(|node| node.label.as_str())
            .collect::<Vec<_>>();

        assert_eq!(labels, vec!["nested.log"]);

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
            SourceKind::Archive(ArchiveFormat::Tar)
        ));

        let nested_report = loader.load_children(&inner_node);
        assert!(
            nested_report.errors.is_empty(),
            "ZIP 内层 TAR 应能展开：{:?}",
            nested_report.errors
        );
        let labels = nested_report
            .registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|id| nested_report.registry.node(*id))
            .map(|node| node.label.as_str())
            .collect::<Vec<_>>();

        assert_eq!(labels, vec!["nested.log"]);

        let _ = fs::remove_dir_all(&root);
    }
}
