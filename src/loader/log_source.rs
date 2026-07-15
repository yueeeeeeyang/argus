//! 文件职责：定义日志来源树的核心数据模型。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：统一描述本地文件、目录、压缩包和压缩包内虚拟条目。

use std::fmt;
use std::path::PathBuf;

use crate::loader::archive::detector::ArchiveFormat;

/// 来源节点稳定 ID，UI 通过该 ID 选择、展开和滚动定位节点。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct SourceId(pub usize);

impl fmt::Display for SourceId {
    /// 将来源 ID 输出为稳定数字文本，便于 GPUI 元素 ID 拼接。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// 来源节点类型，决定图标、可展开能力和状态文案。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SourceKind {
    /// 本地目录。
    Directory,
    /// 普通日志候选文件。
    LogFile,
    /// 可展开或受限的压缩包文件。
    Archive(ArchiveFormat),
    /// 根层仅包含一个普通文件的压缩包；界面显示压缩包名，但点击直接打开内部文件。
    SingleFileArchive(ArchiveFormat),
    /// 压缩包内目录。
    ArchiveDirectory,
    /// 压缩包内文件条目。
    ArchiveFile,
    /// 当前识别但暂不支持的来源。
    Unsupported(String),
}

impl SourceKind {
    /// 返回节点是否拥有可展开子级。
    pub(crate) fn can_expand(&self) -> bool {
        matches!(
            self,
            Self::Directory | Self::Archive(_) | Self::ArchiveDirectory
        )
    }

    /// 返回节点是否表示用户可选择的日志候选。
    pub(crate) fn is_log_candidate(&self) -> bool {
        matches!(
            self,
            Self::LogFile | Self::ArchiveFile | Self::SingleFileArchive(_)
        )
    }
}

/// 来源位置，区分真实本地路径和压缩包内部虚拟路径。
#[derive(Clone, Debug)]
pub(crate) enum SourceLocation {
    /// 本地文件或目录路径。
    LocalPath(PathBuf),
    /// 压缩包内部条目路径。
    ArchiveEntry {
        /// 外层压缩包真实路径。
        archive_path: PathBuf,
        /// 最外层真实压缩包格式，用于从本地文件启动嵌套读取链路。
        root_format: ArchiveFormat,
        /// 从外层压缩包到当前容器之间的嵌套压缩包条目链路。
        container_entries: Vec<String>,
        /// 内部条目路径，统一使用 `/` 分隔。
        entry_path: String,
        /// 当前条目所属容器的压缩格式。
        format: ArchiveFormat,
        /// 嵌套压缩包深度。
        archive_depth: usize,
    },
}

impl SourceLocation {
    /// 返回面向状态栏展示的位置文本。
    pub(crate) fn display_path(&self) -> String {
        match self {
            Self::LocalPath(path) => path.display().to_string(),
            Self::ArchiveEntry {
                archive_path,
                container_entries,
                entry_path,
                ..
            } => crate::utils::path::archive_virtual_path(
                archive_path,
                container_entries,
                entry_path,
            ),
        }
    }
}

/// 来源节点元信息，不包含文件句柄或日志正文。
#[derive(Clone, Debug, Default)]
pub(crate) struct SourceMetadata {
    /// 文件或条目大小。
    pub size: Option<u64>,
    /// 是否已完成子级加载。
    pub children_loaded: bool,
    /// 是否正在后台加载子级。
    pub is_loading: bool,
    /// 加载失败或能力受限说明。
    pub message: Option<String>,
}

/// 来源树节点；树关系由注册表集中维护，节点自身只保存父级 ID。
#[derive(Clone, Debug)]
pub(crate) struct SourceTreeNode {
    /// 节点稳定 ID。
    pub id: SourceId,
    /// 父节点 ID；根节点为 `None`。
    pub parent_id: Option<SourceId>,
    /// 节点层级，用于 UI 缩进和连线。
    pub depth: usize,
    /// 界面展示名称。
    pub label: String,
    /// 来源类型。
    pub kind: SourceKind,
    /// 来源位置。
    pub location: SourceLocation,
    /// 节点元信息。
    pub metadata: SourceMetadata,
    /// 是否选中。
    pub selected: bool,
    /// 是否展开。
    pub expanded: bool,
}
