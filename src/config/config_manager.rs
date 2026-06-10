//! 文件职责：提供应用配置管理入口。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：集中创建运行期默认配置，为后续持久化配置预留边界。

use crate::config::app_config::AppConfig;

/// 配置管理器，当前只返回内存默认配置，不读写用户配置文件。
#[derive(Debug, Default)]
pub struct ConfigManager;

impl ConfigManager {
    /// 加载应用配置。
    ///
    /// 返回值：当前阶段始终返回默认配置；后续接入配置文件时在此处处理读取、校验和迁移。
    pub fn load_default() -> AppConfig {
        AppConfig::default()
    }
}
