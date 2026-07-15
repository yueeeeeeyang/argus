//! 文件职责：导出日志来源加载模块。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：提供日志来源模型、来源注册表、目录树加载器和压缩包适配器。

pub(crate) mod archive;
pub(crate) mod dir_tree;
pub(crate) mod log_source;
pub(crate) mod path_browser;
pub(crate) mod source_registry;

pub(crate) use dir_tree::{
    LoadReport, LogSourceLoader, SourceArchiveProbeRequest, SourceArchiveProbeResult,
};
pub(crate) use log_source::{SourceId, SourceKind, SourceLocation, SourceMetadata, SourceTreeNode};
pub(crate) use path_browser::{
    BrowseEntry, BrowseEntryKind, BrowseLocation, BrowseResult, PathBrowser,
};
pub(crate) use source_registry::SourceRegistry;
