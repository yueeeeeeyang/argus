//! 文件职责：提供自定义日志来源选择器使用的跨平台文件系统浏览服务。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：枚举本地目录、识别可加载来源类型、生成常用位置入口。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context as _, Result, bail};

use crate::config::paths::user_home_dir;
use crate::loader::archive::{ArchiveFormat, archive_registry};
use crate::utils::path::display_name;

/// 自定义选择器左侧快捷入口。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowseLocation {
    /// 展示文案，例如“主目录”或“下载”。
    pub label: String,
    /// 入口对应的真实本地路径。
    pub path: PathBuf,
}

/// 文件系统条目类型，决定选择器行图标、点击行为和可选状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowseEntryKind {
    /// 本地目录；目录行用于进入浏览，不直接通过行选中。
    Directory,
    /// 普通文件；与当前来源加载器保持一致，全部视为日志候选。
    LogFile,
    /// 已注册并支持展开的压缩包文件。
    Archive(ArchiveFormat),
    /// 能识别但当前来源树不支持的压缩包格式。
    UnsupportedArchive(String),
    /// 非普通文件或其他无法作为日志来源的条目。
    OtherUnsupported,
}

impl BrowseEntryKind {
    /// 返回当前条目是否可以被选择为加载来源。
    pub fn is_selectable(&self) -> bool {
        matches!(self, Self::LogFile | Self::Archive(_))
    }

    /// 返回选择器中展示的简短类型文案。
    pub fn label(&self) -> String {
        match self {
            Self::Directory => "目录".to_string(),
            Self::LogFile => "文件".to_string(),
            Self::Archive(format) => format.label().to_string(),
            Self::UnsupportedArchive(reason) => reason.clone(),
            Self::OtherUnsupported => "不可选".to_string(),
        }
    }
}

/// 文件系统浏览结果中的单个条目。
#[derive(Clone, Debug)]
pub struct BrowseEntry {
    /// 条目真实路径，后续确认选择时直接交给来源加载器。
    pub path: PathBuf,
    /// 条目展示名称。
    pub name: String,
    /// 条目分类。
    pub kind: BrowseEntryKind,
    /// 文件大小；目录和无法读取大小的条目为 `None`。
    pub size: Option<u64>,
    /// 文件修改时间；读取失败时为 `None`。
    pub modified: Option<SystemTime>,
    /// 是否可作为来源加入选择列表。
    pub is_selectable: bool,
    /// 禁用原因；用于不可选条目提示。
    pub disabled_reason: Option<String>,
}

/// 单次目录枚举结果。
#[derive(Clone, Debug)]
pub struct BrowseResult {
    /// 已规范化的当前目录路径。
    pub directory: PathBuf,
    /// 当前目录的父目录；没有父级时为 `None`。
    pub parent: Option<PathBuf>,
    /// 当前目录直接子项，已按目录优先和名称排序。
    pub entries: Vec<BrowseEntry>,
}

/// 跨平台本地文件系统浏览服务。
pub struct PathBrowser;

impl PathBrowser {
    /// 枚举指定目录的直接子项。
    ///
    /// 参数说明：
    /// - `path`：要浏览的目录路径，可为相对路径或绝对路径。
    ///
    /// 返回值：目录自身、父目录和已分类排序的直接子项。
    ///
    /// 可能错误：路径不存在、不是目录、无权限或读取目录项失败。
    pub fn list_directory(path: PathBuf) -> Result<BrowseResult> {
        let directory = normalize_existing_directory(&path)?;
        let mut entries = Vec::new();

        for entry in fs::read_dir(&directory)
            .with_context(|| format!("无法读取目录：{}", directory.display()))?
        {
            let entry =
                entry.with_context(|| format!("无法读取目录项：{}", directory.display()))?;
            entries.push(Self::snapshot_entry(entry.path()));
        }

        entries.sort_by(|left, right| {
            let left_group = if matches!(left.kind, BrowseEntryKind::Directory) {
                0
            } else {
                1
            };
            let right_group = if matches!(right.kind, BrowseEntryKind::Directory) {
                0
            } else {
                1
            };

            left_group
                .cmp(&right_group)
                .then_with(|| {
                    left.name
                        .to_ascii_lowercase()
                        .cmp(&right.name.to_ascii_lowercase())
                })
                .then_with(|| left.name.cmp(&right.name))
        });

        Ok(BrowseResult {
            parent: directory.parent().map(Path::to_path_buf),
            directory,
            entries,
        })
    }

    /// 返回选择器首次打开时应定位的目录。
    pub fn default_start_directory() -> PathBuf {
        user_home_dir()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// 返回跨平台常用位置入口；不存在的位置会被过滤。
    pub fn default_locations() -> Vec<BrowseLocation> {
        let mut locations = Vec::new();

        if let Some(home) = user_home_dir() {
            push_location(&mut locations, "主目录", home.clone());
            push_location(&mut locations, "桌面", home.join("Desktop"));
            push_location(&mut locations, "下载", home.join("Downloads"));
            push_location(&mut locations, "文档", home.join("Documents"));
        }

        if cfg!(windows) {
            for drive in b'A'..=b'Z' {
                let path = PathBuf::from(format!("{}:\\", drive as char));
                if path.exists() {
                    push_location(&mut locations, &format!("{} 盘", drive as char), path);
                }
            }
        } else {
            push_location(&mut locations, "根目录", PathBuf::from("/"));
        }

        locations
    }

    /// 根据路径生成一个带选择能力说明的条目快照。
    fn snapshot_entry(path: PathBuf) -> BrowseEntry {
        let metadata = fs::metadata(&path).or_else(|_| fs::symlink_metadata(&path));
        let name = display_name(&path);

        let (kind, size, modified) = match metadata {
            Ok(metadata) if metadata.is_dir() => {
                (BrowseEntryKind::Directory, None, metadata.modified().ok())
            }
            Ok(metadata) if metadata.is_file() => {
                let kind = classify_file_entry(&path);
                (kind, Some(metadata.len()), metadata.modified().ok())
            }
            Ok(metadata) => (
                BrowseEntryKind::OtherUnsupported,
                Some(metadata.len()),
                metadata.modified().ok(),
            ),
            Err(_) => (BrowseEntryKind::OtherUnsupported, None, None),
        };
        let is_selectable = kind.is_selectable();
        let disabled_reason = disabled_reason_for_kind(&kind);

        BrowseEntry {
            path,
            name,
            kind,
            size,
            modified,
            is_selectable,
            disabled_reason,
        }
    }
}

/// 将目录路径规范化为可读目录；失败时保留原始路径用于错误提示。
fn normalize_existing_directory(path: &Path) -> Result<PathBuf> {
    if !path.exists() {
        bail!("路径不存在：{}", path.display());
    }
    if !path.is_dir() {
        bail!("不是目录：{}", path.display());
    }

    fs::canonicalize(path).or_else(|_| Ok(path.to_path_buf()))
}

/// 根据来源树注册表规则识别文件条目类型。
fn classify_file_entry(path: &Path) -> BrowseEntryKind {
    match archive_registry().detect_path(path) {
        Some(format) if format.is_supported() => BrowseEntryKind::Archive(format),
        Some(format) => BrowseEntryKind::UnsupportedArchive(format.label().to_string()),
        None => BrowseEntryKind::LogFile,
    }
}

/// 返回不可选条目的用户可理解原因。
fn disabled_reason_for_kind(kind: &BrowseEntryKind) -> Option<String> {
    match kind {
        BrowseEntryKind::Directory => Some("单击选中目录，双击进入目录".to_string()),
        BrowseEntryKind::UnsupportedArchive(reason) => {
            Some(format!("当前来源树不支持该压缩格式：{reason}"))
        }
        BrowseEntryKind::OtherUnsupported => Some("该条目不是普通文件或目录".to_string()),
        BrowseEntryKind::LogFile | BrowseEntryKind::Archive(_) => None,
    }
}

/// 去重后加入一个存在的常用位置。
fn push_location(locations: &mut Vec<BrowseLocation>, label: &str, path: PathBuf) {
    if !path.exists() || !path.is_dir() || locations.iter().any(|location| location.path == path) {
        return;
    }

    locations.push(BrowseLocation {
        label: label.to_string(),
        path,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    /// 为路径浏览测试创建隔离临时目录。
    fn temp_root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("argus-path-browser-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("应能创建测试目录");
        root
    }

    /// 创建一个最小 ZIP 文件，验证选择器复用压缩注册表识别能力。
    fn write_zip(path: &Path) {
        let file = fs::File::create(path).expect("应能创建 ZIP 文件");
        let mut writer = ZipWriter::new(file);
        writer
            .start_file("app.log", SimpleFileOptions::default())
            .expect("应能创建 ZIP 条目");
        writer.write_all(b"INFO").expect("应能写入 ZIP 条目");
        writer.finish().expect("应能完成 ZIP 文件");
    }

    /// 验证目录浏览按目录优先和名称排序。
    #[test]
    fn list_directory_sorts_directories_before_files() {
        let root = temp_root("sorts-directories-before-files");
        fs::write(root.join("z.log"), b"z").expect("应能写入文件");
        fs::create_dir(root.join("alpha")).expect("应能创建目录");
        fs::write(root.join("a.log"), b"a").expect("应能写入文件");

        let result = PathBrowser::list_directory(root).expect("应能浏览目录");
        let names = result
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["alpha", "a.log", "z.log"]);
        assert!(matches!(result.entries[0].kind, BrowseEntryKind::Directory));
    }

    /// 验证普通文件可作为直接可选来源，目录由选择器 UI 通过专用单击逻辑处理。
    #[test]
    fn plain_files_are_selectable_and_directories_use_ui_selection() {
        let root = temp_root("plain-files-are-selectable");
        fs::create_dir(root.join("logs")).expect("应能创建目录");
        fs::write(root.join("app.log"), b"INFO").expect("应能写入文件");

        let result = PathBrowser::list_directory(root).expect("应能浏览目录");
        let directory = result
            .entries
            .iter()
            .find(|entry| entry.name == "logs")
            .expect("应存在目录");
        let log_file = result
            .entries
            .iter()
            .find(|entry| entry.name == "app.log")
            .expect("应存在日志文件");

        assert!(!directory.is_selectable);
        assert!(log_file.is_selectable);
        assert!(matches!(log_file.kind, BrowseEntryKind::LogFile));
    }

    /// 验证支持的压缩包文件会被识别为可选归档来源。
    #[test]
    fn supported_archive_file_is_selectable() {
        let root = temp_root("supported-archive-file");
        write_zip(&root.join("logs.zip"));

        let result = PathBrowser::list_directory(root).expect("应能浏览目录");
        let archive = result
            .entries
            .iter()
            .find(|entry| entry.name == "logs.zip")
            .expect("应存在 ZIP 文件");

        assert!(archive.is_selectable);
        assert!(matches!(
            archive.kind,
            BrowseEntryKind::Archive(ArchiveFormat::Zip)
        ));
    }

    /// 验证手动输入非目录时返回明确错误。
    #[test]
    fn list_directory_rejects_non_directory_path() {
        let root = temp_root("rejects-non-directory");
        let file = root.join("app.log");
        fs::write(&file, b"INFO").expect("应能写入文件");

        let error = PathBrowser::list_directory(file)
            .expect_err("文件路径不应被当作目录浏览")
            .to_string();

        assert!(error.contains("不是目录"));
    }

    /// 验证跨平台常用位置至少包含一个存在的入口。
    #[test]
    fn default_locations_only_include_existing_directories() {
        for location in PathBrowser::default_locations() {
            assert!(location.path.exists());
            assert!(location.path.is_dir());
        }
    }
}
