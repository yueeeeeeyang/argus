//! 文件职责：导出日志来源加载模块。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：提供日志来源模型、来源注册表、目录树加载器和压缩包适配器。

pub mod archive;
pub mod credential_store;
pub mod dir_tree;
pub mod file_watcher;
pub mod log_source;
pub mod path_browser;
pub mod source_registry;
pub mod spool_manager;

pub use dir_tree::{
    LoadReport, LogSourceLoader, SourceArchiveProbePatch, SourceArchiveProbeRequest,
    SourceArchiveProbeResult,
};
pub use log_source::{SourceId, SourceKind, SourceLocation, SourceMetadata, SourceTreeNode};
pub use path_browser::{BrowseEntry, BrowseEntryKind, BrowseLocation, BrowseResult, PathBrowser};
pub use source_registry::SourceRegistry;
