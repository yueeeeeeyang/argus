//! 文件职责：基础设施模块入口，聚合自动升级、性能计时和文本选择等底层工具能力。
//! 创建日期：2026-07-08
//! 作者：Argus 开发团队
//! 主要功能：统一导出升级服务、性能计时和文本选择三个子模块。

pub mod perf;
pub mod text_selection;
pub mod updater;
