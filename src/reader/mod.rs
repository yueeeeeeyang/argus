//! 文件职责：导出日志读取模块的占位边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：为读取器、后端、文本显示、编码检测和增量解码预留模块结构。

pub(crate) mod decoder;
pub(crate) mod encoding_detector;
pub(crate) mod line_index;
pub(crate) mod log_file_reader;
pub(crate) mod mmap_backend;
pub(crate) mod page_info;
pub(crate) mod read_mode;
pub(crate) mod spooled_backend;
pub(crate) mod stream_backend;
