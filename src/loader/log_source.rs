//! 文件职责：定义日志来源树的核心数据模型。
//! 创建日期：2026-06-09
//! 修改日期：2026-09-12
//! 作者：Argus 开发团队
//! 主要功能：统一描述工作目录中的目录、普通日志文件、加密压缩包占位和暂不支持来源。

use std::fmt;
use std::path::PathBuf;

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
    /// 目录。
    Directory,
    /// 普通日志候选文件。
    LogFile,
    /// 加密压缩包占位：物化时因缺少密码或密码错误未展开，点击引导输入密码后追加物化。
    ArchivePasswordRequired,
    /// 当前识别但暂不支持的来源（符号链接、不可读、未展开的压缩包等）。
    Unsupported(String),
}

impl SourceKind {
    /// 返回节点是否拥有可展开子级。
    pub(crate) fn can_expand(&self) -> bool {
        matches!(self, Self::Directory)
    }

    /// 返回节点是否表示用户可选择的日志候选。
    pub(crate) fn is_log_candidate(&self) -> bool {
        matches!(self, Self::LogFile)
    }
}

/// 来源位置。
///
/// 说明：工作目录物化后全部来源都是普通文件路径；保留枚举形式是为了让既有
/// `SourceLocation::LocalPath(...)` 解构和 `display_path` 展示调用点零改动。
#[derive(Clone, Debug)]
pub(crate) enum SourceLocation {
    /// 本地文件或目录路径（工作目录内物化文件，或密码占位指向的原始压缩包）。
    LocalPath(PathBuf),
}

impl SourceLocation {
    /// 返回面向状态栏展示的位置文本。
    pub(crate) fn display_path(&self) -> String {
        match self {
            Self::LocalPath(path) => path.display().to_string(),
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
