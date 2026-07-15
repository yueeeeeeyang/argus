//! 文件职责：导出日志读取模块。
//! 创建日期：2026-06-09
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：组织日志读取器、索引、编码检测及多种存储后端。

pub(crate) mod encoding_detector;
pub(crate) mod line_index;
pub(crate) mod log_file_reader;
pub(crate) mod mmap_backend;
pub(crate) mod spooled_backend;
pub(crate) mod stream_backend;
