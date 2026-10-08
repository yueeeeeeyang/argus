//! 文件职责：提供界面阅读器与 AI Agent 共同依赖的日志底层 I/O 能力。
//! 创建日期：2026-08-05
//! 修改日期：2026-10-05
//! 作者：Argus 开发团队
//! 主要功能：集中编码检测、行索引、内存映射和统一日志文档实现。

pub(crate) mod encoding_detector;
pub(crate) mod line_index;
pub(crate) mod log_file_reader;
pub(crate) mod mmap_backend;
