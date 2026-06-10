//! 文件职责：集中管理 Argus 用户配置目录路径。
//! 创建日期：2026-06-10
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：提供 `~/.argus`、主题目录和设置文件路径，避免路径规则散落在业务模块中。

use std::path::{Path, PathBuf};

/// Argus 用户配置目录名称。
const ARGUS_CONFIG_DIR_NAME: &str = ".argus";
/// Argus 用户主题目录名称。
const ARGUS_THEME_DIR_NAME: &str = "themes";
/// Argus 用户设置文件名称。
const ARGUS_SETTINGS_FILE_NAME: &str = "settings.toml";

/// 返回当前用户的 Argus 配置目录。
///
/// 返回值：优先使用 `HOME` 环境变量拼接 `~/.argus`；若运行环境缺少 HOME，则回退到当前目录下的 `.argus`。
pub fn argus_config_dir() -> PathBuf {
    home_dir()
        .map(|home| argus_config_dir_from_home(&home))
        .unwrap_or_else(|| PathBuf::from(ARGUS_CONFIG_DIR_NAME))
}

/// 返回当前用户的 Argus 主题目录。
///
/// 返回值：固定为 `~/.argus/themes`，用于读取用户自定义 TOML 主题。
pub fn argus_theme_dir() -> PathBuf {
    argus_theme_dir_from_config(&argus_config_dir())
}

/// 返回当前用户的 Argus 设置文件路径。
///
/// 返回值：固定为 `~/.argus/settings.toml`，用于持久化外观、加载、编码和缓存设置。
pub fn argus_settings_file() -> PathBuf {
    argus_settings_file_from_config(&argus_config_dir())
}

/// 根据指定 home 目录构造 Argus 配置目录，供单元测试避免依赖真实用户目录。
pub fn argus_config_dir_from_home(home: &Path) -> PathBuf {
    home.join(ARGUS_CONFIG_DIR_NAME)
}

/// 根据指定配置目录构造主题目录，供主题管理器和测试复用。
pub fn argus_theme_dir_from_config(config_dir: &Path) -> PathBuf {
    config_dir.join(ARGUS_THEME_DIR_NAME)
}

/// 根据指定配置目录构造设置文件路径，供配置管理器和测试复用。
pub fn argus_settings_file_from_config(config_dir: &Path) -> PathBuf {
    config_dir.join(ARGUS_SETTINGS_FILE_NAME)
}

/// 获取用户 home 目录；独立成函数便于说明路径回退策略。
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证配置目录严格落在指定 home 下的 `.argus`。
    #[test]
    fn config_dir_from_home_uses_argus_directory() {
        let home = PathBuf::from("/tmp/argus-home");

        assert_eq!(
            argus_config_dir_from_home(&home),
            PathBuf::from("/tmp/argus-home/.argus")
        );
    }

    /// 验证主题目录和设置文件路径都从同一个配置目录派生。
    #[test]
    fn theme_and_settings_paths_share_config_root() {
        let config_dir = PathBuf::from("/tmp/argus-home/.argus");

        assert_eq!(
            argus_theme_dir_from_config(&config_dir),
            PathBuf::from("/tmp/argus-home/.argus/themes")
        );
        assert_eq!(
            argus_settings_file_from_config(&config_dir),
            PathBuf::from("/tmp/argus-home/.argus/settings.toml")
        );
    }
}
