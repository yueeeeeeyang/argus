//! 文件职责：为现有界面业务保留日志读取模块的兼容导出。
//! 创建日期：2026-06-09
//! 修改日期：2026-08-05
//! 作者：Argus 开发团队
//! 主要功能：从共享 `log_io` 基础层导出日志读取器、索引、编码检测及多种存储后端。

pub(crate) use crate::log_io::encoding_detector;
pub(crate) use crate::log_io::log_file_reader;
pub(crate) use crate::log_io::stream_backend;
