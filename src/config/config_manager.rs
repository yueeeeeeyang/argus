//! 文件职责：提供应用配置读写管理入口。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-12
//! 作者：Argus 开发团队
//! 主要功能：从 `~/.argus/settings.toml` 读取设置，并以原子写入方式持久化用户修改。

use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::config::app_config::AppConfig;
use crate::config::paths::argus_settings_file;

/// 配置读写错误，调用方可据此显示非阻塞提示。
#[derive(Debug, Error)]
pub enum ConfigError {
    /// 配置文件读写失败。
    #[error("配置文件 IO 失败：{0}")]
    Io(#[from] std::io::Error),
    /// 配置文件 TOML 解析失败。
    #[error("配置文件解析失败：{0}")]
    Parse(#[from] toml::de::Error),
    /// 配置文件 TOML 序列化失败。
    #[error("配置文件序列化失败：{0}")]
    Serialize(#[from] toml::ser::Error),
}

/// 配置管理器，持有当前设置文件路径，便于生产和测试环境复用同一套读写逻辑。
#[derive(Clone, Debug)]
pub struct ConfigManager {
    /// 当前配置文件路径，生产环境固定为 `~/.argus/settings.toml`。
    settings_path: PathBuf,
}

impl ConfigManager {
    /// 构造使用默认用户配置路径的配置管理器。
    pub fn default_paths() -> Self {
        Self::new(argus_settings_file())
    }

    /// 构造指定设置文件路径的配置管理器。
    ///
    /// 参数说明：
    /// - `settings_path`：设置文件路径，测试可注入临时目录避免污染真实用户配置。
    pub fn new(settings_path: PathBuf) -> Self {
        Self { settings_path }
    }

    /// 兼容旧调用方的配置加载入口。
    ///
    /// 返回值：读取默认路径的配置，失败时回退默认值。
    pub fn load_default() -> AppConfig {
        Self::default_paths().load()
    }

    /// 从当前管理器路径读取配置。
    ///
    /// 返回值：文件不存在或解析失败时返回默认配置，保证应用启动不被坏配置阻塞。
    pub fn load(&self) -> AppConfig {
        self.load_with_warning().0
    }

    /// 从当前管理器路径读取配置，并返回非阻塞 warning。
    ///
    /// 返回值：第一项为可用配置，第二项为坏配置或 IO 异常导致回退默认值时的说明。
    pub fn load_with_warning(&self) -> (AppConfig, Option<String>) {
        match Self::load_from_path(&self.settings_path) {
            Ok(config) => (config, None),
            Err(error) => (
                AppConfig::default(),
                Some(format!(
                    "读取设置文件 {} 失败，已使用默认设置：{error}",
                    self.settings_path.display()
                )),
            ),
        }
    }

    /// 将配置保存到当前管理器路径。
    ///
    /// 返回值：写入失败时返回错误，调用方负责显示提示但不回滚 UI 状态。
    pub fn save(&self, config: &AppConfig) -> Result<(), ConfigError> {
        Self::save_to_path(&self.settings_path, config)
    }

    /// 从指定路径读取配置，供单元测试和未来迁移逻辑复用。
    pub fn load_from_path(path: &Path) -> Result<AppConfig, ConfigError> {
        if !path.exists() {
            return Ok(AppConfig::default());
        }

        let text = fs::read_to_string(path)?;
        let config = toml::from_str::<AppConfig>(&text)?;
        Ok(config.normalized())
    }

    /// 将配置写入指定路径，先写临时文件再 rename，降低异常退出造成半文件的概率。
    pub fn save_to_path(path: &Path, config: &AppConfig) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let normalized_config = config.clone().normalized();
        let text = toml::to_string_pretty(&normalized_config)?;
        let temp_path = path.with_extension("toml.tmp");
        fs::write(&temp_path, text)?;
        fs::rename(temp_path, path)?;
        Ok(())
    }

    /// 返回当前设置文件路径，便于测试和诊断输出确认真实落点。
    pub fn settings_path(&self) -> &Path {
        &self.settings_path
    }
}

impl Default for ConfigManager {
    /// 构造默认配置管理器，生产环境使用 `~/.argus/settings.toml`。
    fn default() -> Self {
        Self::default_paths()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AppearanceConfig, CacheConfig, EncodingConfig, LoaderConfig, LogSearchConfig,
    };

    /// 构造唯一测试配置路径，避免并发测试之间互相覆盖。
    fn test_settings_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "argus-config-test-{}-{name}/settings.toml",
            std::process::id()
        ))
    }

    /// 验证设置文件不存在时会返回默认配置。
    #[test]
    fn missing_settings_file_loads_default_config() {
        let path = test_settings_path("missing");
        let _ = fs::remove_file(&path);

        let config = ConfigManager::load_from_path(&path).expect("缺失配置文件应回退默认配置");

        assert_eq!(config.appearance.theme_mode, "dark.toml");
        assert_eq!(config.loader.max_archive_depth, 2);
    }

    /// 验证保存后再次读取可以恢复用户设置。
    #[test]
    fn save_then_load_round_trips_settings() {
        let path = test_settings_path("round-trip");
        let config = AppConfig {
            appearance: AppearanceConfig {
                theme_mode: "custom_dark.toml".to_string(),
                log_content_font_size: 16.0,
            },
            loader: LoaderConfig {
                max_archive_depth: 4,
                archive_probe_concurrency: 6,
                follow_symlinks: true,
            },
            log_search: LogSearchConfig {
                quick_keywords: "ERROR,WARN".to_string(),
            },
            encoding: EncodingConfig {
                selected: "GBK".to_string(),
            },
            cache: CacheConfig {
                enabled: false,
                limit_mb: 1024,
            },
        };

        ConfigManager::save_to_path(&path, &config).expect("测试配置应可写入临时目录");
        let loaded = ConfigManager::load_from_path(&path).expect("测试配置应可再次读取");

        assert_eq!(loaded.appearance.theme_mode, "custom_dark.toml");
        assert_eq!(loaded.appearance.log_content_font_size, 16.0);
        assert_eq!(loaded.loader.max_archive_depth, 4);
        assert_eq!(loaded.loader.archive_probe_concurrency, 6);
        assert!(loaded.loader.follow_symlinks);
        assert_eq!(loaded.log_search.quick_keywords, "ERROR,WARN");
        assert_eq!(loaded.encoding.selected, "GBK");
        assert!(!loaded.cache.enabled);
        assert_eq!(loaded.cache.limit_mb, 1024);
    }

    /// 验证坏 TOML 会暴露解析错误，让默认加载入口决定是否回退。
    #[test]
    fn invalid_settings_file_returns_parse_error() {
        let path = test_settings_path("invalid");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("测试目录应可创建");
        }
        fs::write(&path, "not = [valid").expect("测试坏配置应可写入");

        let error = ConfigManager::load_from_path(&path).expect_err("坏 TOML 应返回解析错误");

        assert!(matches!(error, ConfigError::Parse(_)));
    }

    /// 验证默认加载入口遇到坏配置时会回退默认配置并返回 warning。
    #[test]
    fn load_with_warning_falls_back_on_invalid_config() {
        let path = test_settings_path("invalid-warning");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("测试目录应可创建");
        }
        fs::write(&path, "bad = [").expect("测试坏配置应可写入");
        let manager = ConfigManager::new(path);

        let (config, warning) = manager.load_with_warning();

        assert_eq!(config.appearance.theme_mode, "dark.toml");
        assert!(warning.is_some());
    }
}
