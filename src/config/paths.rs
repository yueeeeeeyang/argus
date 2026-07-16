//! 文件职责：集中管理 Argus 用户配置目录路径。
//! 创建日期：2026-06-10
//! 修改日期：2026-07-16
//! 作者：Argus 开发团队
//! 主要功能：提供生产配置路径及仓库内隔离的 `.argus_test` 测试路径，避免路径规则散落在业务模块中。

use std::ffi::OsString;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::OnceLock;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};

/// Argus 用户配置目录名称。
const ARGUS_CONFIG_DIR_NAME: &str = ".argus";
/// Argus 测试数据根目录名称；测试构建不得读写生产 `.argus` 目录。
#[cfg(test)]
const ARGUS_TEST_DIR_NAME: &str = ".argus_test";
/// Argus 用户主题目录名称。
const ARGUS_THEME_DIR_NAME: &str = "themes";
/// Argus 用户设置文件名称。
const ARGUS_SETTINGS_FILE_NAME: &str = "settings.toml";
/// Argus 升级缓存目录名称。
const ARGUS_UPDATES_DIR_NAME: &str = "updates";
/// Argus 内置仓库缓存目录名称。
const ARGUS_REPOSITORIES_DIR_NAME: &str = "repositories";
/// Git 裸仓库缓存子目录名称。
const ARGUS_GIT_REPOSITORIES_DIR_NAME: &str = "git";
/// SVN SSH 主机公钥记录文件名称。
const ARGUS_SVN_KNOWN_HOSTS_FILE_NAME: &str = "svn_known_hosts";
/// 当前测试进程的唯一目录，保证同一用户并发运行测试时互不覆盖。
#[cfg(test)]
static ARGUS_TEST_PROCESS_DIR: OnceLock<PathBuf> = OnceLock::new();
/// 测试子目录序号，保证同一进程内的并行用例也使用独立目录。
#[cfg(test)]
static NEXT_TEST_DIR_ID: AtomicUsize = AtomicUsize::new(0);

/// 返回当前用户的 Argus 配置目录。
///
/// 返回值：生产构建使用 `~/.argus`；测试构建固定使用当前测试进程专属的
/// `<项目根>/.argus_test/<进程标识>/config`，从路径入口阻断对生产配置的访问。
pub(crate) fn argus_config_dir() -> PathBuf {
    #[cfg(test)]
    {
        argus_test_process_dir().join("config")
    }

    #[cfg(not(test))]
    {
        user_home_dir()
            .map(|home| argus_config_dir_from_home(&home))
            .unwrap_or_else(|| PathBuf::from(ARGUS_CONFIG_DIR_NAME))
    }
}

/// 返回测试数据根目录，所有会写文件的单元测试都应落在该目录下。
///
/// 返回值：固定为 Cargo 项目根下的 `.argus_test`，既避免访问用户生产目录，也能在
/// CI 和文件系统沙箱中稳定写入。
#[cfg(test)]
pub(crate) fn argus_test_root_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(ARGUS_TEST_DIR_NAME)
}

/// 返回本次测试进程独占的工作目录。
///
/// 进程号之外再加入启动时间，避免操作系统复用进程号时读到上次测试遗留的数据。
#[cfg(test)]
pub(crate) fn argus_test_process_dir() -> PathBuf {
    ARGUS_TEST_PROCESS_DIR
        .get_or_init(|| {
            let started_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            argus_test_root_dir().join(format!("process-{}-{started_at}", std::process::id()))
        })
        .clone()
}

/// 为单个测试场景分配唯一工作目录。
///
/// 参数说明：
/// - `scope`：简短的测试场景名称，仅用于提高遗留目录的可辨识度。
///
/// 返回值：位于当前进程测试目录下且不会与其他并行用例冲突的路径。
#[cfg(test)]
pub(crate) fn isolated_test_dir(scope: &str) -> PathBuf {
    let safe_scope = scope
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    let id = NEXT_TEST_DIR_ID.fetch_add(1, Ordering::Relaxed);
    argus_test_process_dir().join(format!("{safe_scope}-{id}"))
}

/// 为需要直接读写普通文件的测试创建独立父目录并返回文件路径。
///
/// 参数说明：
/// - `scope`：测试场景名称；
/// - `file_name`：测试文件名，不得依赖生产目录中的同名文件。
///
/// 返回值：位于唯一 `.argus_test` 子目录中的文件路径。
#[cfg(test)]
pub(crate) fn isolated_test_file_path(scope: &str, file_name: &str) -> PathBuf {
    let directory = isolated_test_dir(scope);
    std::fs::create_dir_all(&directory).expect("应创建独立的 .argus_test 测试目录");
    directory.join(file_name)
}

/// 创建会在作用域结束时自动清理的独立测试目录。
///
/// 参数说明：
/// - `scope`：测试场景名称，用于生成易于定位的目录前缀。
///
/// 返回值：位于当前 `.argus_test` 进程目录中的 `TempDir`；创建失败时直接终止测试。
#[cfg(test)]
pub(crate) fn temporary_test_dir(scope: &str) -> tempfile::TempDir {
    let process_dir = argus_test_process_dir();
    std::fs::create_dir_all(&process_dir).expect("应创建 .argus_test 测试进程目录");
    tempfile::Builder::new()
        .prefix(&format!("{scope}-"))
        .tempdir_in(process_dir)
        .expect("应在 .argus_test 中创建临时测试目录")
}

/// 断言测试文件路径位于 `.argus_test` 根目录内。
///
/// 该保护只存在于测试构建中，并由配置读写入口调用；一旦新测试误传
/// `~/.argus/settings.toml` 或系统临时目录，测试会在实际 IO 前立即失败。
#[cfg(test)]
pub(crate) fn assert_isolated_test_path(path: &Path) {
    let test_root = argus_test_root_dir();
    assert!(
        path.starts_with(&test_root),
        "测试文件必须位于独立的 {} 目录，禁止访问生产目录或通用临时目录：{}",
        test_root.display(),
        path.display()
    );
}

/// 返回当前用户的 Argus 主题目录。
///
/// 返回值：固定为 `~/.argus/themes`，用于读取用户自定义 TOML 主题。
pub(crate) fn argus_theme_dir() -> PathBuf {
    argus_theme_dir_from_config(&argus_config_dir())
}

/// 返回当前用户的 Argus 设置文件路径。
///
/// 返回值：固定为 `~/.argus/settings.toml`，用于持久化外观、加载、编码和缓存设置。
pub(crate) fn argus_settings_file() -> PathBuf {
    argus_settings_file_from_config(&argus_config_dir())
}

/// 返回当前用户的 Argus 升级缓存目录。
///
/// 返回值：固定为 `~/.argus/updates`，用于保存下载完成且已校验的升级二进制。
pub(crate) fn argus_updates_dir() -> PathBuf {
    argus_updates_dir_from_config(&argus_config_dir())
}

/// 返回 Git 持久裸仓库缓存根目录。
///
/// 返回值：固定为 `~/.argus/repositories/git`，每个 Git 链接在其下使用独立目录。
pub(crate) fn argus_git_repositories_dir() -> PathBuf {
    argus_git_repositories_dir_from_config(&argus_config_dir())
}

/// 返回 SVN Rust SSH 栈专用的 known_hosts 文件路径。
///
/// 该文件与用户的 `~/.ssh/known_hosts` 隔离，避免读取系统 SSH 配置或默认身份。
pub(crate) fn argus_svn_known_hosts_file() -> PathBuf {
    argus_svn_known_hosts_file_from_config(&argus_config_dir())
}

/// 根据指定 home 目录构造 Argus 配置目录，供单元测试避免依赖真实用户目录。
pub(crate) fn argus_config_dir_from_home(home: &Path) -> PathBuf {
    home.join(ARGUS_CONFIG_DIR_NAME)
}

/// 根据指定配置目录构造主题目录，供主题管理器和测试复用。
pub(crate) fn argus_theme_dir_from_config(config_dir: &Path) -> PathBuf {
    config_dir.join(ARGUS_THEME_DIR_NAME)
}

/// 根据指定配置目录构造设置文件路径，供配置管理器和测试复用。
pub(crate) fn argus_settings_file_from_config(config_dir: &Path) -> PathBuf {
    config_dir.join(ARGUS_SETTINGS_FILE_NAME)
}

/// 根据指定配置目录构造升级缓存目录，供升级模块和测试复用。
pub(crate) fn argus_updates_dir_from_config(config_dir: &Path) -> PathBuf {
    config_dir.join(ARGUS_UPDATES_DIR_NAME)
}

/// 根据指定配置目录构造 Git 裸仓库缓存根目录，供实现与测试复用。
pub(crate) fn argus_git_repositories_dir_from_config(config_dir: &Path) -> PathBuf {
    config_dir
        .join(ARGUS_REPOSITORIES_DIR_NAME)
        .join(ARGUS_GIT_REPOSITORIES_DIR_NAME)
}

/// 根据指定配置目录构造 SVN 专用 known_hosts 路径，供实现与测试复用。
pub(crate) fn argus_svn_known_hosts_file_from_config(config_dir: &Path) -> PathBuf {
    config_dir.join(ARGUS_SVN_KNOWN_HOSTS_FILE_NAME)
}

/// 获取用户 home 目录；独立成函数便于说明跨平台路径回退策略。
/// 获取用户 home 目录，供配置、主题和跨平台文件选择器复用。
pub(crate) fn user_home_dir() -> Option<PathBuf> {
    non_empty_env("HOME")
        .map(PathBuf::from)
        .or_else(|| non_empty_env("USERPROFILE").map(PathBuf::from))
        .or_else(|| {
            windows_home_from_drive_and_path(non_empty_env("HOMEDRIVE"), non_empty_env("HOMEPATH"))
        })
}

/// 读取非空环境变量，避免空字符串被误当作有效路径。
fn non_empty_env(key: &str) -> Option<OsString> {
    std::env::var_os(key).filter(|value| !value.is_empty())
}

/// 根据 Windows 的 `HOMEDRIVE` 与 `HOMEPATH` 组合用户目录。
fn windows_home_from_drive_and_path(
    home_drive: Option<OsString>,
    home_path: Option<OsString>,
) -> Option<PathBuf> {
    let mut home = home_drive?;
    home.push(home_path?);
    Some(PathBuf::from(home))
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

    /// 验证测试构建的默认配置不会指向生产 `.argus` 目录。
    #[test]
    fn default_config_dir_is_isolated_under_argus_test() {
        let config_dir = argus_config_dir();

        assert!(config_dir.starts_with(argus_test_root_dir()));
        assert!(config_dir.ends_with("config"));
        assert!(
            !config_dir.starts_with(
                user_home_dir()
                    .map(|home| argus_config_dir_from_home(&home))
                    .unwrap_or_else(|| PathBuf::from(ARGUS_CONFIG_DIR_NAME))
            )
        );
    }

    /// 验证每个测试场景都会取得不同的 `.argus_test` 子目录。
    #[test]
    fn isolated_test_dirs_are_unique_and_share_test_root() {
        let first = isolated_test_dir("paths");
        let second = isolated_test_dir("paths");

        assert_ne!(first, second);
        assert!(first.starts_with(argus_test_root_dir()));
        assert!(second.starts_with(argus_test_root_dir()));
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
        assert_eq!(
            argus_updates_dir_from_config(&config_dir),
            PathBuf::from("/tmp/argus-home/.argus/updates")
        );
    }

    /// 验证 Windows 分离的 home 环境变量可以组合成用户目录。
    #[test]
    fn windows_home_parts_are_combined() {
        let home = windows_home_from_drive_and_path(
            Some(OsString::from("C:")),
            Some(OsString::from("\\Users\\argus")),
        )
        .expect("Windows home 片段应能组合");

        assert_eq!(home, PathBuf::from("C:\\Users\\argus"));
    }

    /// 验证 Windows 缺少任一 home 片段时不会产生半截路径。
    #[test]
    fn windows_home_parts_require_drive_and_path() {
        assert!(windows_home_from_drive_and_path(Some(OsString::from("C:")), None).is_none());
        assert!(
            windows_home_from_drive_and_path(None, Some(OsString::from("\\Users\\argus")))
                .is_none()
        );
    }
}
