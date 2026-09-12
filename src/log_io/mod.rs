//! 文件职责：提供界面阅读器与 AI Agent 共同依赖的日志底层 I/O 能力。
//! 创建日期：2026-08-05
//! 修改日期：2026-09-12
//! 作者：Argus 开发团队
//! 主要功能：集中编码检测、行索引、内存映射和统一日志文档实现。

// 底层实现仍保留原文件位置以控制本次重构范围；模块所有权迁到 `log_io` 后，业务层只依赖
// 这一共享基础边界，`reader` 仅为现有界面调用保留兼容导出。
#[path = "../reader/encoding_detector.rs"]
pub(crate) mod encoding_detector;
#[path = "../reader/line_index.rs"]
pub(crate) mod line_index;
#[path = "../reader/log_file_reader.rs"]
pub(crate) mod log_file_reader;
#[path = "../reader/mmap_backend.rs"]
pub(crate) mod mmap_backend;
