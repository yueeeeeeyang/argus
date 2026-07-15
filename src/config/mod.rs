//! 文件职责：导出配置管理模块。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-18
//! 作者：Argus 开发团队
//! 主要功能：提供应用配置模型、日志搜索配置、日志显示配置、升级配置和配置管理入口。

pub(crate) mod app_config;
pub(crate) mod config_manager;
pub(crate) mod paths;

pub(crate) use app_config::{AppConfig, LoaderConfig, SEARCH_RECENT_KEYWORDS_MAX, UpgradeConfig};
pub(crate) use config_manager::ConfigManager;
