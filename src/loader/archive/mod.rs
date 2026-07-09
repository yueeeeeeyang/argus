//! 文件职责：导出压缩包适配器模块。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：提供格式检测、统一条目模型、能力注册表和各压缩格式适配器。

pub mod adapter;
pub mod compressed_tar;
pub mod detector;
pub mod gzip_adapter;
pub mod password;
pub mod rar_adapter;
pub mod registry;
pub mod sevenz_adapter;
pub mod tar_adapter;
pub mod unsupported;
pub mod zip_adapter;

pub use adapter::{
    ArchiveCapabilities, ArchiveEntryConsumer, ArchiveEntryInfo, ArchiveRootProbe,
    list_archive_entries, stream_archive_entry, stream_archive_entry_with_passwords,
};
pub use detector::{ArchiveFormat, detect_archive_format, detect_archive_format_by_name};
pub use password::{
    ArchivePasswordError, ArchivePasswordErrorKind, ArchivePasswordKey, ArchivePasswordStore,
    find_archive_password_error,
};
pub use registry::{ArchiveAdapterRegistry, archive_registry};
