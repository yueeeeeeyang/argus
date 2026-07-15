//! 文件职责：提供应用配置读写管理入口。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-18
//! 作者：Argus 开发团队
//! 主要功能：从 `~/.argus/settings.toml` 读取设置，并以原子写入方式持久化用户修改。

use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::config::app_config::AppConfig;
use crate::config::paths::argus_settings_file;

/// 配置读写错误，调用方可据此显示非阻塞提示。
#[derive(Debug, Error)]
pub(crate) enum ConfigError {
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
pub(crate) struct ConfigManager {
    /// 当前配置文件路径，生产环境固定为 `~/.argus/settings.toml`。
    settings_path: PathBuf,
}

impl ConfigManager {
    /// 构造使用默认用户配置路径的配置管理器。
    pub(crate) fn default_paths() -> Self {
        Self::new(argus_settings_file())
    }

    /// 构造指定设置文件路径的配置管理器。
    ///
    /// 参数说明：
    /// - `settings_path`：设置文件路径，测试可注入临时目录避免污染真实用户配置。
    pub(crate) fn new(settings_path: PathBuf) -> Self {
        Self { settings_path }
    }

    /// 从当前管理器路径读取配置。
    ///
    /// 返回值：文件不存在或解析失败时返回默认配置，保证应用启动不被坏配置阻塞。
    #[cfg(test)]
    pub(crate) fn load(&self) -> AppConfig {
        self.load_with_warning().0
    }

    /// 从当前管理器路径读取配置，并返回非阻塞 warning。
    ///
    /// 返回值：第一项为可用配置，第二项为坏配置或 IO 异常导致回退默认值时的说明。
    pub(crate) fn load_with_warning(&self) -> (AppConfig, Option<String>) {
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
    pub(crate) fn save(&self, config: &AppConfig) -> Result<(), ConfigError> {
        Self::save_to_path(&self.settings_path, config)
    }

    /// 从指定路径读取配置，供单元测试和未来迁移逻辑复用。
    pub(crate) fn load_from_path(path: &Path) -> Result<AppConfig, ConfigError> {
        if !path.exists() {
            return Ok(AppConfig::default());
        }

        let text = fs::read_to_string(path)?;
        let config = toml::from_str::<AppConfig>(&text)?;
        Ok(config.normalized())
    }

    /// 将配置写入指定路径，先写临时文件再 rename，降低异常退出造成半文件的概率。
    pub(crate) fn save_to_path(path: &Path, config: &AppConfig) -> Result<(), ConfigError> {
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

    /// 返回当前设置文件路径，供与配置同根目录的缓存和测试数据保持隔离。
    ///
    /// 返回值：创建该管理器时指定的 `settings.toml` 路径，不执行文件系统访问。
    pub(crate) fn settings_path(&self) -> &Path {
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
    use crate::config::app_config::{
        AppearanceConfig, DEFAULT_JSTACK_STACK_SEGMENT_FILTERS, DEFAULT_JSTACK_THREAD_NAME_FILTERS,
        EncodingConfig, LoaderConfig, LogDisplayConfig, LogSearchConfig, UpgradeConfig,
    };
    use crate::remote::connection::{
        ConnectionConfig, ConnectionDirectoryConfig, ConnectionLinkConfig, SmbLinkConfig,
        SshLinkConfig, TrustedHostKeyConfig,
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
        assert_eq!(
            config.log_display.jstack_thread_name_filters,
            DEFAULT_JSTACK_THREAD_NAME_FILTERS
        );
        assert_eq!(
            config.log_display.jstack_stack_segment_filters,
            DEFAULT_JSTACK_STACK_SEGMENT_FILTERS
        );
    }

    /// 验证旧配置缺少 Jstack 过滤字段时会补齐默认过滤，避免升级后设置页出现空值。
    #[test]
    fn missing_log_display_filter_fields_load_default_filters() {
        let path = test_settings_path("missing-log-display-fields");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("测试目录应可创建");
        }
        fs::write(&path, "[log_display]\n").expect("测试旧配置应可写入");

        let config = ConfigManager::load_from_path(&path).expect("旧配置应可使用字段默认值读取");

        assert_eq!(
            config.log_display.jstack_thread_name_filters,
            DEFAULT_JSTACK_THREAD_NAME_FILTERS
        );
        assert_eq!(
            config.log_display.jstack_stack_segment_filters,
            DEFAULT_JSTACK_STACK_SEGMENT_FILTERS
        );
    }

    /// 验证协议化连接配置仍能读取旧版本保存的 SSH 链接字段。
    #[test]
    fn legacy_ssh_connection_config_loads_as_ssh_link() {
        let path = test_settings_path("legacy-ssh-connection");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("测试目录应可创建");
        }
        fs::write(
            &path,
            r#"
[connections]
next_id = 2

[[connections.links]]
id = 1
name = "legacy-ssh"

[connections.links.ssh]
host = "10.0.0.8"
port = 2202
username = "deploy"
password = " secret "
private_key_path = "/Users/yueyang/.ssh/id_ed25519"
private_key_passphrase = " phrase "
"#,
        )
        .expect("测试旧 SSH 配置应可写入");

        let config = ConfigManager::load_from_path(&path).expect("旧 SSH 配置应可读取");
        let link = config
            .connections
            .links
            .first()
            .expect("旧 SSH 链接应被加载");
        let ssh = link.ssh.as_ref().expect("旧链接应识别为 SSH");

        assert!(link.smb.is_none());
        assert_eq!(ssh.host, "10.0.0.8");
        assert_eq!(ssh.port, 2202);
        assert_eq!(ssh.password, " secret ");
        assert_eq!(
            ssh.private_key_path.as_deref(),
            Some("/Users/yueyang/.ssh/id_ed25519")
        );
        assert_eq!(ssh.private_key_passphrase.as_deref(), Some(" phrase "));
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
                recent_keywords: Vec::new(),
            },
            log_display: LogDisplayConfig {
                jstack_thread_name_filters: "Attach Listener,Signal Dispatcher".to_string(),
                jstack_stack_segment_filters: "Unsafe.park\n\nSocketInputStream\\nread".to_string(),
            },
            connections: ConnectionConfig {
                next_id: 4,
                directories: vec![ConnectionDirectoryConfig {
                    id: 1,
                    parent_id: None,
                    name: "生产环境".to_string(),
                    expanded: true,
                }],
                links: vec![
                    ConnectionLinkConfig {
                        id: 2,
                        parent_id: Some(1),
                        name: "app-01".to_string(),
                        ssh: Some(SshLinkConfig {
                            host: "10.0.0.1".to_string(),
                            port: 22,
                            username: "deploy".to_string(),
                            password: "secret".to_string(),
                            private_key_path: Some("/Users/yueyang/.ssh/id_ed25519".to_string()),
                            private_key_passphrase: Some("phrase".to_string()),
                        }),
                        smb: None,
                        git: None,
                        svn: None,
                    },
                    ConnectionLinkConfig {
                        id: 3,
                        parent_id: Some(1),
                        name: "share-01".to_string(),
                        ssh: None,
                        smb: Some(SmbLinkConfig {
                            host: "10.0.0.2".to_string(),
                            port: 445,
                            share: "logs".to_string(),
                            initial_dir: "/runtime".to_string(),
                            domain: Some("WORKGROUP".to_string()),
                            username: "smbuser".to_string(),
                            password: " smb-secret ".to_string(),
                        }),
                        git: None,
                        svn: None,
                    },
                ],
                trusted_hosts: vec![TrustedHostKeyConfig {
                    host: "10.0.0.1".to_string(),
                    port: 22,
                    fingerprint: "SHA256:test".to_string(),
                }],
            },
            encoding: EncodingConfig {
                selected: "GBK".to_string(),
            },
            upgrade: UpgradeConfig {
                enabled: true,
                server_url: "https://updates.example.com/argus".to_string(),
                public_key_base64: "TEST_PUBLIC_KEY_BASE64".to_string(),
                skipped_version: Some("0.2.0".to_string()),
                last_check_at: Some("2026-06-15T12:00:00Z".to_string()),
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
        assert_eq!(
            loaded.log_display.jstack_thread_name_filters,
            "Attach Listener,Signal Dispatcher"
        );
        assert_eq!(
            loaded.log_display.jstack_stack_segment_filters,
            "Unsafe.park\n\nSocketInputStream\\nread"
        );
        assert_eq!(loaded.connections.directories[0].name, "生产环境");
        let ssh = loaded.connections.links[0].ssh.as_ref().unwrap();
        assert_eq!(ssh.password, "secret");
        assert_eq!(ssh.private_key_passphrase.as_deref(), Some("phrase"));
        let smb = loaded.connections.links[1].smb.as_ref().unwrap();
        assert_eq!(smb.share, "logs");
        assert_eq!(smb.password, " smb-secret ");
        assert_eq!(
            loaded.connections.trusted_hosts[0].fingerprint,
            "SHA256:test"
        );
        assert_eq!(loaded.encoding.selected, "GBK");
        assert!(loaded.upgrade.enabled);
        assert_eq!(
            loaded.upgrade.server_url,
            "https://updates.example.com/argus"
        );
        assert_eq!(loaded.upgrade.public_key_base64, "TEST_PUBLIC_KEY_BASE64");
        assert_eq!(loaded.upgrade.skipped_version.as_deref(), Some("0.2.0"));
    }

    /// 验证旧版无效缓存配置可以被忽略，并在下一次保存时从设置文件中清除。
    #[test]
    fn legacy_cache_section_is_ignored_and_removed_on_save() {
        let path = test_settings_path("legacy-cache");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("旧配置迁移测试目录应可创建");
        }
        let mut config = AppConfig::default();
        config.encoding.selected = "GBK".to_string();
        let mut text = toml::to_string_pretty(&config).expect("默认配置应可序列化");
        text.push_str("\n[cache]\nenabled = false\nlimit_mb = 1024\n");
        fs::write(&path, text).expect("应能写入带旧缓存段的设置文件");

        let loaded = ConfigManager::load_from_path(&path).expect("旧缓存段不应阻断配置加载");
        assert_eq!(loaded.encoding.selected, "GBK");

        ConfigManager::save_to_path(&path, &loaded).expect("迁移后的配置应可保存");
        let saved = fs::read_to_string(&path).expect("应能读取迁移后的配置");
        assert!(!saved.contains("[cache]"));
        assert!(saved.contains("selected = \"GBK\""));
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
