//! 文件职责：导出日志读取模块的占位边界。
//! 创建日期：2026-06-09
//! 作者：Argus 开发团队
//! 主要功能：为读取器、后端、文本显示、编码检测和增量解码预留模块结构。

pub mod backend;
pub mod decoder;
pub mod encoding_detector;
pub mod line_index;
pub mod log_file_reader;
pub mod mmap_backend;
pub mod page_info;
pub mod read_mode;
pub mod spooled_backend;
pub mod stream_backend;
