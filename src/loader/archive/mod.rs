//! 文件职责：导出压缩包适配器模块。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：提供格式检测、统一条目模型、能力注册表和各压缩格式适配器。

pub(crate) mod adapter;
pub(crate) mod compressed_tar;
pub(crate) mod detector;
pub(crate) mod gzip_adapter;
pub(crate) mod password;
pub(crate) mod rar_adapter;
pub(crate) mod registry;
pub(crate) mod sevenz_adapter;
pub(crate) mod tar_adapter;
pub(crate) mod unsupported;
pub(crate) mod zip_adapter;

pub(crate) use adapter::{ArchiveEntryConsumer, stream_archive_entry_with_passwords};
pub(crate) use detector::{ArchiveFormat, detect_archive_format};
pub(crate) use password::{
    ArchivePasswordError, ArchivePasswordErrorKind, ArchivePasswordKey, ArchivePasswordStore,
    find_archive_password_error,
};
pub(crate) use registry::archive_registry;
