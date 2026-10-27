//! 文件职责：导出日志来源加载模块。
//! 创建日期：2026-06-09
//! 修改日期：2026-10-08
//! 作者：Argus 开发团队
//! 主要功能：提供日志来源模型、来源注册表、来源树全量扫描器、压缩包适配器和单文件导出。

pub(crate) mod archive;
pub(crate) mod export;
pub(crate) mod log_source;
pub(crate) mod path_browser;
pub(crate) mod source_registry;
pub(crate) mod source_scanner;
pub(crate) mod workspace;

pub(crate) use export::export_source_file;
pub(crate) use log_source::{SourceId, SourceKind, SourceLocation, SourceMetadata, SourceTreeNode};
pub(crate) use path_browser::{
    BrowseEntry, BrowseEntryKind, BrowseLocation, BrowseResult, PathBrowser,
};
pub(crate) use source_registry::SourceRegistry;
pub(crate) use source_scanner::{
    SourceLoadPhase, SourceTreeScanProgress, SourceTreeScanResult, SourceTreeScanner,
};
pub(crate) use workspace::MaterializedWorkspace;
