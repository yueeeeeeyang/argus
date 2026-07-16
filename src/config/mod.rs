//! 文件职责：导出配置管理模块。
//! 创建日期：2026-06-09
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：提供 AI 分析、应用、日志搜索、日志显示、升级配置和配置管理入口。

pub(crate) mod ai_config;
pub(crate) mod app_config;
pub(crate) mod config_manager;
pub(crate) mod paths;

pub(crate) use ai_config::{
    AI_RAW_LOG_CONSENT_VERSION, AiConfig, AiModelProfile, DEFAULT_AI_SYSTEM_PROMPT, LogNameMatcher,
    LogNameMatcherMode, LogNameMatcherTarget, LogTypeProfile,
};
pub(crate) use app_config::{AppConfig, LoaderConfig, SEARCH_RECENT_KEYWORDS_MAX, UpgradeConfig};
pub(crate) use config_manager::ConfigManager;
