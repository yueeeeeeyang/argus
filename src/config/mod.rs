//! 文件职责：导出配置管理模块。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：提供应用配置模型和配置管理入口。

pub mod app_config;
pub mod config_manager;

pub use app_config::{AppConfig, LoaderConfig};
pub use config_manager::ConfigManager;
