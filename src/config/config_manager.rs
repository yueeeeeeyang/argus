//! 文件职责：提供应用配置读写管理入口。
//! 创建日期：2026-06-09
//! 修改日期：2026-09-24
//! 作者：Argus 开发团队
//! 主要功能：从 `~/.argus/settings.toml` 读取设置，并以原子写入方式持久化用户修改。

use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::config::app_config::AppConfig;
use crate::config::paths::argus_settings_file;
#[cfg(test)]
use crate::config::paths::assert_isolated_test_path;
use crate::config::secret_store::{
    SecretWriteOutcome, account_for_link, forget_secret, load_secret, store_secret,
};
use crate::remote::connection::ConnectionSecretSlot;

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
    /// 连接机密无法保存；调用方应中止保存并提示用户，而不是丢弃密码。
    #[error("{0}")]
    Secret(String),
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
    /// - `settings_path`：设置文件路径，测试必须注入 `.argus_test` 子目录避免污染真实用户配置。
    pub(crate) fn new(settings_path: PathBuf) -> Self {
        #[cfg(test)]
        assert_isolated_test_path(&settings_path);
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
    /// 返回值：第一项为可用配置，第二项为坏配置、IO 异常或凭据迁移问题导致的说明。
    pub(crate) fn load_with_warning(&self) -> (AppConfig, Option<String>) {
        match Self::load_from_path(&self.settings_path) {
            Ok((config, warnings)) => (config, join_config_warnings(warnings)),
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

    /// 从指定路径读取配置，并把凭据回填到内存机密字段。
    ///
    /// 返回值：第一项为可用配置，第二项为非致命的凭据读取说明（例如凭据库不可用）。
    /// 只有配置文件自身的 IO 或解析错误才会返回 `Err`；凭据问题不会让用户丢失全部设置。
    pub(crate) fn load_from_path(path: &Path) -> Result<(AppConfig, Vec<String>), ConfigError> {
        #[cfg(test)]
        assert_isolated_test_path(path);
        if !path.exists() {
            return Ok((AppConfig::default(), Vec::new()));
        }

        let text = fs::read_to_string(path)?;
        let mut config = toml::from_str::<AppConfig>(&text)?;
        // 必须先回填机密再规范化：设置文件里已经不含密码，若先规范化，校验会判定
        // “SMB 链接缺少密码”并丢弃用户链接。
        let warnings = hydrate_connection_secrets(&mut config);
        Ok((config.normalized(), warnings))
    }

    /// 将配置写入指定路径，先写临时文件再 rename，降低异常退出造成半文件的概率。
    ///
    /// 机密处理：写入前把所有连接机密交给系统凭据库（或本地加密回落），设置文件中只保留
    /// 凭据库不可用时的密文；凭据写入失败会让整个保存失败，绝不静默丢弃用户密码。
    pub(crate) fn save_to_path(path: &Path, config: &AppConfig) -> Result<(), ConfigError> {
        #[cfg(test)]
        assert_isolated_test_path(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut normalized_config = config.clone().normalized();
        persist_connection_secrets(&mut normalized_config)?;
        let text = toml::to_string_pretty(&normalized_config)?;

        let temp_path = path.with_extension("toml.tmp");
        fs::write(&temp_path, text)?;
        fs::rename(temp_path, path)?;
        // 设置文件现在可能包含密文或凭据账户名，收紧为仅所有者可读写。
        restrict_settings_file_permissions(path)?;
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

/// 把多条非致命说明合并成一条可展示文本。
fn join_config_warnings(warnings: Vec<String>) -> Option<String> {
    if warnings.is_empty() {
        None
    } else {
        Some(warnings.join("；"))
    }
}

/// 从凭据库（或本地密文）把连接机密回填到内存字段，并迁移旧版明文。
///
/// 迁移语义：旧版本把密码明文写在 `settings.toml`；这里读到明文后会立即写入凭据库，
/// 由于机密字段已标记 `skip_serializing`，明文会在下一次保存时从文件中彻底消失。
///
/// 说明：迁移成功后**保留**内存中的明文值。运行时连接（SSH 终端、SFTP、SMB 等）直接读取
/// 这些字段，提前清空会让用户在本次会话中无法连接。
///
/// 返回值：需要展示给用户的非致命说明列表。
fn hydrate_connection_secrets(config: &mut AppConfig) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut migrated_link_ids = Vec::new();

    for link in &mut config.connections.links {
        let link_id = link.id;
        for (slot, value) in link.secret_slots() {
            let account = account_for_link(link_id, slot.as_str());
            // 旧明文优先迁移：内存里已有值且凭据库中没有对应条目时写回凭据库。
            if let Some(plaintext) = value {
                match store_secret(&account, &plaintext) {
                    Ok(SecretWriteOutcome::Keyring) => {
                        migrated_link_ids.push(link_id);
                    }
                    Ok(SecretWriteOutcome::LocalCiphertext(ciphertext)) => {
                        config
                            .connections
                            .secrets
                            .insert(account.clone(), ciphertext);
                        migrated_link_ids.push(link_id);
                    }
                    Err(error) => {
                        // 迁移失败必须保留原值并如实上报，绝不能静默丢弃用户密码。
                        warnings.push(format!(
                            "连接“{}”的{}未能迁移到系统凭据库：{error}",
                            link.name,
                            secret_slot_label(slot)
                        ));
                    }
                }
                continue;
            }

            let local_ciphertext = config.connections.secrets.get(&account).map(String::as_str);
            match load_secret(&account, local_ciphertext) {
                Ok(Some(secret)) => {
                    use secrecy::ExposeSecret as _;
                    link.set_secret_slot(slot, Some(secret.expose_secret().to_string()));
                }
                Ok(None) => {}
                Err(error) => warnings.push(format!(
                    "连接“{}”的{}读取失败：{error}",
                    link.name,
                    secret_slot_label(slot)
                )),
            }
        }
    }

    migrated_link_ids.sort_unstable();
    migrated_link_ids.dedup();
    if !migrated_link_ids.is_empty() {
        warnings.push(format!(
            "已把 {} 个连接的历史明文密码迁移到系统凭据库，设置文件中的明文将在下次保存后移除",
            migrated_link_ids.len()
        ));
    }

    warnings
}

/// 把所有连接机密交给凭据库或本地加密回落，并清空内存明文。
///
/// 返回值：任何一条机密无法保存时返回错误，调用方中止整次保存，避免用户以为已保存成功。
fn persist_connection_secrets(config: &mut AppConfig) -> Result<(), ConfigError> {
    let mut secrets = std::collections::BTreeMap::new();

    for link in &mut config.connections.links {
        let link_id = link.id;
        for (slot, value) in link.secret_slots() {
            let account = account_for_link(link_id, slot.as_str());
            match value {
                Some(plaintext) => match store_secret(&account, &plaintext) {
                    Ok(SecretWriteOutcome::Keyring) => {}
                    Ok(SecretWriteOutcome::LocalCiphertext(ciphertext)) => {
                        secrets.insert(account, ciphertext);
                    }
                    Err(error) => {
                        return Err(ConfigError::Secret(format!(
                            "连接“{}”的{}保存失败：{error}",
                            link.name,
                            secret_slot_label(slot)
                        )));
                    }
                },
                // 槽位被清空时同步删除凭据库条目，避免删除连接后凭据仍留在系统中。
                None => {
                    forget_secret(&account).map_err(ConfigError::Secret)?;
                }
            }
            link.set_secret_slot(slot, None);
        }
    }

    config.connections.secrets = secrets;
    Ok(())
}

/// 返回槽位的中文名称，用于面向用户的提示文案。
fn secret_slot_label(slot: ConnectionSecretSlot) -> &'static str {
    match slot {
        ConnectionSecretSlot::SshPassword => "SSH 密码",
        ConnectionSecretSlot::SshKeyPassphrase => "SSH 私钥口令",
        ConnectionSecretSlot::SmbPassword => "SMB 密码",
        ConnectionSecretSlot::GitToken => "Git 访问令牌",
        ConnectionSecretSlot::GitKeyPassphrase => "Git 私钥口令",
        ConnectionSecretSlot::SvnPassword => "SVN 密码",
        ConnectionSecretSlot::SvnKeyPassphrase => "SVN 私钥口令",
    }
}

/// 把设置文件收紧为仅所有者可读写。
///
/// 设置文件保存着凭据账户名与本地密文，默认的 0644 会让同机其他账户读到这些内容。
#[cfg(unix)]
fn restrict_settings_file_permissions(path: &Path) -> Result<(), ConfigError> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Windows 的 ACL 继承自用户配置目录，无需额外收紧。
#[cfg(not(unix))]
fn restrict_settings_file_permissions(_path: &Path) -> Result<(), ConfigError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::app_config::{
        AppearanceConfig, EncodingConfig, JstackThreadFilterRule, JstackThreadFilterRuleKind,
        LoaderConfig, LogDisplayConfig, LogSearchConfig,
    };
    use crate::config::paths::{argus_config_dir_from_home, isolated_test_dir, user_home_dir};
    use crate::remote::connection::{
        ConnectionConfig, ConnectionDirectoryConfig, ConnectionLinkConfig, SmbLinkConfig,
        SshLinkConfig, TrustedHostKeyConfig,
    };

    /// 构造唯一测试配置路径，避免并发测试之间互相覆盖。
    fn test_settings_path(name: &str) -> PathBuf {
        isolated_test_dir(&format!("config-manager-{name}")).join("settings.toml")
    }

    /// 验证无法恢复密码的连接不会被静默丢弃。
    ///
    /// 场景：设置文件来自其他机器（本地密文无法解密），或系统凭据库暂时不可用。此时链接必须
    /// 仍然保留在配置中并给出明确提示，否则用户会在毫无察觉的情况下失去整条连接配置。
    #[test]
    fn unrestorable_secret_keeps_link_instead_of_dropping_it() {
        let path = test_settings_path("secret-unrestorable");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("凭据迁移测试目录应可创建");
        }
        // 伪造一条来自其他机器的本地密文：版本前缀合法但内容无法通过认证。
        fs::write(
            &path,
            r#"
[connections]
next_id = 2

[connections.secrets]
"link/1/smb_password" = "v1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"

[[connections.links]]
id = 1
name = "share-01"

[connections.links.smb]
host = "10.0.0.2"
port = 445
share = "logs"
username = "smbuser"
"#,
        )
        .expect("应能写入测试配置");

        let (loaded, warnings) =
            ConfigManager::load_from_path(&path).expect("配置应可加载而不报错");
        assert_eq!(
            loaded.connections.links.len(),
            1,
            "密码无法恢复时链接必须保留，不能被静默丢弃"
        );
        assert!(
            warnings.iter().any(|warning| warning.contains("读取失败")),
            "无法恢复密码必须如实上报：{warnings:?}"
        );
    }

    /// 验证历史明文密码会被迁移到机密存储，并在下一次保存后从设置文件中彻底消失。
    ///
    /// 这是本次凭据改造的核心回归点：既不能继续把明文写回文件，也不能让用户在迁移中丢失密码。
    #[test]
    fn legacy_plaintext_credentials_migrate_out_of_settings_file() {
        let path = test_settings_path("secret-migration");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("凭据迁移测试目录应可创建");
        }
        // 模拟旧版本写下的明文配置。
        fs::write(
            &path,
            r#"
[connections]
next_id = 3

[[connections.links]]
id = 1
name = "app-01"

[connections.links.ssh]
host = "10.0.0.1"
port = 22
username = "deploy"
password = "legacy-ssh-password"

[[connections.links]]
id = 2
name = "share-01"

[connections.links.smb]
host = "10.0.0.2"
port = 445
share = "logs"
username = "smbuser"
password = "legacy-smb-password"
"#,
        )
        .expect("应能写入旧版明文配置");

        // 第一次加载：机密被迁移到存储，内存中仍可直接使用（运行时连接依赖该值）。
        let (loaded, warnings) =
            ConfigManager::load_from_path(&path).expect("旧版明文配置应可加载");
        assert_eq!(loaded.connections.links.len(), 2, "迁移不得丢弃连接");
        let ssh = loaded.connections.links[0]
            .ssh
            .as_ref()
            .expect("应保留 SSH");
        assert_eq!(ssh.password, "legacy-ssh-password");
        let smb = loaded.connections.links[1]
            .smb
            .as_ref()
            .expect("应保留 SMB");
        assert_eq!(smb.password, "legacy-smb-password");
        assert!(
            warnings.iter().any(|warning| warning.contains("迁移")),
            "迁移必须如实告知用户：{warnings:?}"
        );

        // 保存后：设置文件中不得再出现任何明文密码。
        ConfigManager::save_to_path(&path, &loaded).expect("迁移后的配置应可保存");
        let saved = fs::read_to_string(&path).expect("应能读取迁移后的配置");
        assert!(
            !saved.contains("legacy-ssh-password"),
            "SSH 明文密码必须已从设置文件移除：{saved}"
        );
        assert!(
            !saved.contains("legacy-smb-password"),
            "SMB 明文密码必须已从设置文件移除：{saved}"
        );
        assert!(saved.contains("app-01"), "非机密字段必须完整保留");

        // 再次加载：密码从机密存储回填，连接依然可用。
        let (reloaded, _warnings) =
            ConfigManager::load_from_path(&path).expect("迁移后配置应可再次加载");
        let ssh = reloaded.connections.links[0]
            .ssh
            .as_ref()
            .expect("应保留 SSH");
        assert_eq!(ssh.password, "legacy-ssh-password");
        let smb = reloaded.connections.links[1]
            .smb
            .as_ref()
            .expect("应保留 SMB");
        assert_eq!(smb.password, "legacy-smb-password");
    }

    /// 验证保存后的设置文件收紧为仅所有者可读写（0600）。
    #[cfg(unix)]
    #[test]
    fn saved_settings_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let path = test_settings_path("secret-permissions");
        ConfigManager::save_to_path(&path, &AppConfig::default()).expect("默认配置应可保存");
        let mode = fs::metadata(&path)
            .expect("应能读取设置文件元信息")
            .permissions()
            .mode();

        assert_eq!(mode & 0o777, 0o600, "设置文件必须仅所有者可读写");
    }

    /// 构造一条启用的线程名过滤规则。
    fn thread_name_rule(pattern: &str) -> JstackThreadFilterRule {
        JstackThreadFilterRule {
            enabled: true,
            kind: JstackThreadFilterRuleKind::ThreadName,
            pattern: pattern.to_string(),
        }
    }

    /// 验证默认配置管理器始终绑定 `.argus_test`，不会读取当前用户的模型配置。
    #[test]
    fn default_manager_uses_isolated_test_settings_file() {
        let manager = ConfigManager::default();

        assert_isolated_test_path(manager.settings_path());
    }

    /// 验证测试代码一旦显式绑定生产设置文件会在执行 IO 前立即失败。
    #[test]
    #[should_panic(expected = "测试文件必须位于独立的")]
    fn production_settings_path_is_rejected_in_tests() {
        let production_root = user_home_dir()
            .map(|home| argus_config_dir_from_home(&home))
            .unwrap_or_else(|| PathBuf::from(".argus"));

        let _manager = ConfigManager::new(production_root.join("settings.toml"));
    }

    /// 验证设置文件不存在时会返回默认配置。
    #[test]
    fn missing_settings_file_loads_default_config() {
        let path = test_settings_path("missing");
        let _ = fs::remove_file(&path);

        let (config, _warnings) =
            ConfigManager::load_from_path(&path).expect("缺失配置文件应回退默认配置");

        assert_eq!(config.appearance.theme_mode, "dark.toml");
        assert_eq!(config.loader.max_archive_depth, 2);
        assert_eq!(
            config.log_display.jstack_thread_filter_rules,
            LogDisplayConfig::default().jstack_thread_filter_rules
        );
        assert!(config.log_display.jstack_thread_name_filters.is_empty());
        assert!(config.log_display.jstack_stack_segment_filters.is_empty());
    }

    /// 验证旧配置缺少 Jstack 过滤规则字段时按空规则列表读取，序列化默认值留给 `Default` 实现。
    #[test]
    fn missing_log_display_filter_fields_load_empty_rules() {
        let path = test_settings_path("missing-log-display-fields");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("测试旧配置应可写入");
        }
        fs::write(&path, "[log_display]\n").expect("测试旧配置应可写入");

        let (config, _warnings) =
            ConfigManager::load_from_path(&path).expect("旧配置应可使用字段默认值读取");

        assert!(config.log_display.jstack_thread_filter_rules.is_empty());
        assert!(config.log_display.jstack_thread_name_filters.is_empty());
        assert!(config.log_display.jstack_stack_segment_filters.is_empty());
    }

    /// 验证旧版自由文本过滤配置在加载时迁移为规则列表，保存后旧字段不再写出。
    #[test]
    fn legacy_jstack_filters_migrate_to_rules_and_drop_legacy_fields() {
        let path = test_settings_path("legacy-jstack-filters");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("迁移测试目录应可创建");
        }
        fs::write(
            &path,
            r#"
[log_display]
jstack_thread_name_filters = "Attach Listener,Signal Dispatcher"
jstack_stack_segment_filters = "Unsafe.park||SocketInputStream\\nread"
"#,
        )
        .expect("旧版过滤配置应可写入");

        let (config, _warnings) =
            ConfigManager::load_from_path(&path).expect("旧版过滤配置应可读取");
        let rules = &config.log_display.jstack_thread_filter_rules;
        assert_eq!(rules.len(), 4);
        assert_eq!(rules[0].kind, JstackThreadFilterRuleKind::ThreadName);
        assert_eq!(rules[0].pattern, "Attach Listener");
        assert_eq!(rules[1].pattern, "Signal Dispatcher");
        assert_eq!(rules[2].kind, JstackThreadFilterRuleKind::StackSegment);
        assert_eq!(rules[2].pattern, "Unsafe.park");
        assert_eq!(rules[3].pattern, "SocketInputStream\nread");
        assert!(config.log_display.jstack_thread_name_filters.is_empty());
        assert!(config.log_display.jstack_stack_segment_filters.is_empty());

        ConfigManager::save_to_path(&path, &config).expect("迁移后的配置应可保存");
        let saved = fs::read_to_string(&path).expect("应能读取迁移后的配置");
        assert!(!saved.contains("jstack_thread_name_filters"));
        assert!(!saved.contains("jstack_stack_segment_filters"));
        assert!(saved.contains("jstack_thread_filter_rules"));
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

        let (config, _warnings) =
            ConfigManager::load_from_path(&path).expect("旧 SSH 配置应可读取");
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
            ai: Default::default(),
            appearance: AppearanceConfig {
                theme_mode: "custom_dark.toml".to_string(),
                log_content_font_size: 16.0,
            },
            loader: LoaderConfig {
                max_archive_depth: 4,
                follow_symlinks: true,
            },
            log_search: LogSearchConfig {
                quick_keywords: "ERROR,WARN".to_string(),
                recent_keywords: Vec::new(),
            },
            log_display: LogDisplayConfig {
                jstack_thread_filter_rules: vec![
                    thread_name_rule("Attach Listener"),
                    JstackThreadFilterRule {
                        enabled: false,
                        kind: JstackThreadFilterRuleKind::StackSegment,
                        pattern: "Unsafe.park\n\nSocketInputStream\\nread".to_string(),
                    },
                ],
                jstack_thread_name_filters: String::new(),
                jstack_stack_segment_filters: String::new(),
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
                secrets: std::collections::BTreeMap::new(),
            },
            encoding: EncodingConfig {
                selected: "GBK".to_string(),
            },
        };

        ConfigManager::save_to_path(&path, &config).expect("测试配置应可写入临时目录");
        let (loaded, _warnings) =
            ConfigManager::load_from_path(&path).expect("测试配置应可再次读取");

        assert_eq!(loaded.appearance.theme_mode, "custom_dark.toml");
        assert_eq!(loaded.appearance.log_content_font_size, 16.0);
        assert_eq!(loaded.loader.max_archive_depth, 4);
        assert!(loaded.loader.follow_symlinks);
        assert_eq!(loaded.log_search.quick_keywords, "ERROR,WARN");
        assert_eq!(
            loaded.log_display.jstack_thread_filter_rules,
            config.log_display.jstack_thread_filter_rules
        );
        assert!(loaded.log_display.jstack_thread_name_filters.is_empty());
        assert!(loaded.log_display.jstack_stack_segment_filters.is_empty());
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

        let (loaded, _warnings) =
            ConfigManager::load_from_path(&path).expect("旧缓存段不应阻断配置加载");
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
