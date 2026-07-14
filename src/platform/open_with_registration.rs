//! 文件职责：封装系统“用 Argus 打开”右键入口的注册与卸载。
//! 创建日期：2026-06-15
//! 修改日期：2026-07-14
//! 作者：Argus 开发团队
//! 主要功能：为 Windows 当前用户注册表和 macOS LaunchServices 提供统一接口。

use anyhow::Result;

/// 系统右键菜单注册状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistrationStatus {
    /// 当前系统已经存在 Argus 右键入口。
    Registered,
    /// 当前系统没有发现 Argus 右键入口。
    NotRegistered,
    /// 当前运行环境不支持注册，并携带用户可读原因。
    Unsupported(String),
    /// 状态无法可靠判断，并携带用户可读原因。
    Unknown(String),
}

impl RegistrationStatus {
    /// 返回是否明确处于已注册状态。
    pub fn is_registered(&self) -> bool {
        matches!(self, Self::Registered)
    }

    /// 返回当前状态是否允许尝试注册。
    pub fn can_register(&self) -> bool {
        !matches!(self, Self::Registered | Self::Unsupported(_))
    }

    /// 返回当前状态是否允许尝试卸载。
    pub fn can_unregister(&self) -> bool {
        !matches!(self, Self::NotRegistered | Self::Unsupported(_))
    }

    /// 返回状态徽标文案。
    pub fn label(&self) -> String {
        match self {
            Self::Registered => "已注册".to_string(),
            Self::NotRegistered => "未注册".to_string(),
            Self::Unsupported(reason) => format!("不可用：{reason}"),
            Self::Unknown(reason) => format!("未知：{reason}"),
        }
    }
}

/// 查询当前平台的系统右键菜单注册状态。
pub fn registration_status() -> RegistrationStatus {
    platform_impl::registration_status()
}

/// 注册系统“用 Argus 打开”入口。
pub fn register_open_with() -> Result<()> {
    platform_impl::register_open_with()
}

/// 卸载系统“用 Argus 打开”入口。
pub fn unregister_open_with() -> Result<()> {
    platform_impl::unregister_open_with()
}

#[cfg(windows)]
mod platform_impl {
    use std::ffi::OsStr;
    use std::iter;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr::{null, null_mut};

    use anyhow::{Context as _, Result, anyhow};
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
        RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    };

    use super::RegistrationStatus;

    /// Windows 当前用户文件右键菜单注册路径。
    const FILE_SHELL_KEY: &str = r"Software\Classes\*\shell\Open with Argus";
    /// Windows 当前用户目录右键菜单注册路径。
    const DIRECTORY_SHELL_KEY: &str = r"Software\Classes\Directory\shell\Open with Argus";
    /// 注册表默认值使用的菜单名称。
    const MENU_LABEL: &str = "用 Argus 打开";

    /// 查询 HKCU 下文件和目录入口是否都已注册。
    pub fn registration_status() -> RegistrationStatus {
        let expected_command = match std::env::current_exe() {
            Ok(exe) => command_for_exe(&exe),
            Err(error) => {
                return RegistrationStatus::Unknown(format!("无法获取当前程序路径：{error}"));
            }
        };

        match (
            read_command_value(FILE_SHELL_KEY),
            read_command_value(DIRECTORY_SHELL_KEY),
        ) {
            (Ok(Some(file_command)), Ok(Some(directory_command)))
                if file_command == expected_command && directory_command == expected_command =>
            {
                RegistrationStatus::Registered
            }
            (Ok(_), Ok(_)) => RegistrationStatus::NotRegistered,
            (Err(error), _) | (_, Err(error)) => RegistrationStatus::Unknown(error.to_string()),
        }
    }

    /// 写入当前用户注册表右键菜单入口；不需要管理员权限。
    pub fn register_open_with() -> Result<()> {
        let exe = std::env::current_exe().context("无法获取当前 Argus 可执行文件路径")?;
        let command = command_for_exe(&exe);

        write_shell_entry(FILE_SHELL_KEY, &exe, &command)?;
        write_shell_entry(DIRECTORY_SHELL_KEY, &exe, &command)?;
        Ok(())
    }

    /// 从当前用户注册表删除 Argus 右键菜单入口。
    pub fn unregister_open_with() -> Result<()> {
        delete_tree(FILE_SHELL_KEY)?;
        delete_tree(DIRECTORY_SHELL_KEY)?;
        Ok(())
    }

    /// 根据可执行文件路径生成注册表 command 字符串。
    pub(crate) fn command_for_exe(exe: &Path) -> String {
        format!("\"{}\" \"%1\"", exe.display())
    }

    /// 写入一个 shell 菜单节点及其 command 子节点。
    fn write_shell_entry(shell_key: &str, exe: &Path, command: &str) -> Result<()> {
        let shell_handle = create_key(shell_key)?;
        set_string_value(shell_handle, None, MENU_LABEL)?;
        set_string_value(shell_handle, Some("Icon"), &exe.display().to_string())?;
        close_key(shell_handle);

        let command_handle = create_key(&format!(r"{shell_key}\command"))?;
        set_string_value(command_handle, None, command)?;
        close_key(command_handle);
        Ok(())
    }

    /// 读取 shell command 默认值。
    fn read_command_value(shell_key: &str) -> Result<Option<String>> {
        let command_key = format!(r"{shell_key}\command");
        let Some(handle) = open_key(&command_key)? else {
            return Ok(None);
        };
        let value = query_default_string(handle)?;
        close_key(handle);
        Ok(value)
    }

    /// 创建或打开注册表键。
    fn create_key(path: &str) -> Result<HKEY> {
        let mut handle: HKEY = null_mut();
        let result = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                wide_null(path).as_ptr(),
                0,
                null_mut(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                null(),
                &mut handle,
                null_mut(),
            )
        };
        ensure_success(result, "创建注册表键失败")?;
        Ok(handle)
    }

    /// 只读打开注册表键，不存在时返回 `Ok(None)`。
    fn open_key(path: &str) -> Result<Option<HKEY>> {
        let mut handle: HKEY = null_mut();
        let result = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                wide_null(path).as_ptr(),
                0,
                KEY_READ,
                &mut handle,
            )
        };
        if result == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        ensure_success(result, "打开注册表键失败")?;
        Ok(Some(handle))
    }

    /// 删除注册表树；键不存在视为已经卸载成功。
    fn delete_tree(path: &str) -> Result<()> {
        let result = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, wide_null(path).as_ptr()) };
        if result == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        ensure_success(result, "删除注册表键失败")
    }

    /// 设置字符串值；`name == None` 表示默认值。
    fn set_string_value(handle: HKEY, name: Option<&str>, value: &str) -> Result<()> {
        let value = wide_null(value);
        let name = name.map(wide_null);
        let result = unsafe {
            RegSetValueExW(
                handle,
                name.as_ref().map_or(null(), |name| name.as_ptr()),
                0,
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * std::mem::size_of::<u16>()) as u32,
            )
        };
        ensure_success(result, "写入注册表字符串失败")
    }

    /// 查询默认字符串值。
    fn query_default_string(handle: HKEY) -> Result<Option<String>> {
        let mut value_type = 0;
        let mut byte_len = 0;
        let result = unsafe {
            RegQueryValueExW(
                handle,
                null(),
                null_mut(),
                &mut value_type,
                null_mut(),
                &mut byte_len,
            )
        };
        if result == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        ensure_success(result, "读取注册表字符串长度失败")?;
        if value_type != REG_SZ || byte_len == 0 {
            return Ok(None);
        }

        let mut buffer = vec![0u16; byte_len as usize / std::mem::size_of::<u16>()];
        let result = unsafe {
            RegQueryValueExW(
                handle,
                null(),
                null_mut(),
                &mut value_type,
                buffer.as_mut_ptr().cast(),
                &mut byte_len,
            )
        };
        ensure_success(result, "读取注册表字符串内容失败")?;
        if buffer.last() == Some(&0) {
            buffer.pop();
        }
        Ok(Some(String::from_utf16_lossy(&buffer)))
    }

    /// 关闭注册表句柄。
    fn close_key(handle: HKEY) {
        unsafe {
            RegCloseKey(handle);
        }
    }

    /// 把 Win32 错误码转换为 anyhow 错误。
    fn ensure_success(result: u32, context: &str) -> Result<()> {
        if result == ERROR_SUCCESS {
            return Ok(());
        }

        Err(anyhow!("{context}，Win32 错误码：{result}"))
    }

    /// 生成以 NUL 结尾的 UTF-16 字符串。
    fn wide_null(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(iter::once(0))
            .collect()
    }
}

#[cfg(target_os = "macos")]
mod platform_impl {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use anyhow::{Context as _, Result, anyhow, bail};

    use super::RegistrationStatus;

    /// macOS LaunchServices 注册工具的固定系统路径。
    const LSREGISTER_PATH: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

    /// 查询 macOS 运行环境是否具备注册条件。
    pub fn registration_status() -> RegistrationStatus {
        match current_app_bundle() {
            Some(app_bundle) => {
                match ensure_bundle_declares_open_with_document_types(&app_bundle) {
                    Ok(()) => RegistrationStatus::Unknown(
                        "可注册；macOS LaunchServices 状态由系统缓存维护".to_string(),
                    ),
                    Err(reason) => RegistrationStatus::Unsupported(reason),
                }
            }
            None => RegistrationStatus::Unsupported(
                "请使用打包后的 Argus.app 运行，cargo run 环境无法注册".to_string(),
            ),
        }
    }

    /// 调用 LaunchServices 注册当前 `.app` bundle。
    pub fn register_open_with() -> Result<()> {
        let app_bundle = current_app_bundle()
            .ok_or_else(|| anyhow!("请使用打包后的 Argus.app 运行，cargo run 环境无法注册"))?;
        ensure_bundle_declares_open_with_document_types(&app_bundle)
            .map_err(|reason| anyhow!(reason))?;
        run_lsregister("-f", &app_bundle).context("注册 macOS Open With 入口失败")
    }

    /// 调用 LaunchServices 反注册当前 `.app` bundle。
    pub fn unregister_open_with() -> Result<()> {
        let app_bundle = current_app_bundle()
            .ok_or_else(|| anyhow!("请使用打包后的 Argus.app 运行，cargo run 环境无法卸载"))?;
        run_lsregister("-u", &app_bundle).context(
            "卸载 macOS Open With 入口失败；若 Finder 仍显示旧入口，请重启 Finder 或等待系统缓存刷新",
        )
    }

    /// 从当前可执行文件路径向上查找 `.app` bundle。
    fn current_app_bundle() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        app_bundle_from_exe_path(&exe)
    }

    /// 判断给定可执行文件路径是否位于 `.app/Contents/MacOS` 内。
    pub(crate) fn app_bundle_from_exe_path(exe: &Path) -> Option<PathBuf> {
        let macos_dir = exe.parent()?;
        if macos_dir.file_name()? != "MacOS" {
            return None;
        }
        let contents_dir = macos_dir.parent()?;
        if contents_dir.file_name()? != "Contents" {
            return None;
        }
        let app_bundle = contents_dir.parent()?;
        let extension = app_bundle.extension()?.to_string_lossy();
        (extension == "app").then(|| app_bundle.to_path_buf())
    }

    /// 校验 `.app` 的 Info.plist 是否声明了可被 Finder “打开方式”识别的文档类型。
    ///
    /// 说明：LaunchServices 只登记 bundle 本身还不够；如果 Info.plist 没有
    /// `CFBundleDocumentTypes`，Finder 可能不会把 Argus 展示为文件/目录可用的打开目标。
    fn ensure_bundle_declares_open_with_document_types(
        app_bundle: &Path,
    ) -> std::result::Result<(), String> {
        let plist_path = app_bundle.join("Contents").join("Info.plist");
        let plist = fs::read_to_string(&plist_path)
            .map_err(|error| format!("无法读取 {}：{error}", plist_path.display()))?;
        if !plist.contains("CFBundleDocumentTypes") {
            return Err(format!(
                "{} 缺少 CFBundleDocumentTypes；请使用 resources/macos/Info.plist 打包 Argus.app",
                plist_path.display()
            ));
        }
        if !plist.contains("public.data") || !plist.contains("public.folder") {
            return Err(format!(
                "{} 未声明 public.data/public.folder，Finder 可能不会显示“用 Argus 打开”",
                plist_path.display()
            ));
        }

        Ok(())
    }

    /// 执行 LaunchServices 注册工具。
    fn run_lsregister(action: &str, app_bundle: &Path) -> Result<()> {
        let output = Command::new(LSREGISTER_PATH)
            .arg(action)
            .arg(app_bundle)
            .output()
            .context("无法启动 LaunchServices 注册工具")?;
        if output.status.success() {
            return Ok(());
        }

        bail!(
            "{}",
            String::from_utf8_lossy(&output.stderr).trim().to_string()
        )
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform_impl {
    use anyhow::{Result, bail};

    use super::RegistrationStatus;

    /// 非 Windows/macOS 平台暂不提供系统右键注册能力。
    pub fn registration_status() -> RegistrationStatus {
        RegistrationStatus::Unsupported("当前平台暂不支持系统右键菜单注册".to_string())
    }

    /// 非 Windows/macOS 平台注册时返回明确错误。
    pub fn register_open_with() -> Result<()> {
        bail!("当前平台暂不支持系统右键菜单注册")
    }

    /// 非 Windows/macOS 平台卸载时返回明确错误。
    pub fn unregister_open_with() -> Result<()> {
        bail!("当前平台暂不支持系统右键菜单卸载")
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// 验证状态到按钮可用性的基础映射。
    #[test]
    fn registration_status_button_rules_are_stable() {
        assert!(RegistrationStatus::NotRegistered.can_register());
        assert!(!RegistrationStatus::NotRegistered.can_unregister());
        assert!(!RegistrationStatus::Registered.can_register());
        assert!(RegistrationStatus::Registered.can_unregister());
        assert!(
            !RegistrationStatus::Unsupported("x".to_string()).can_register(),
            "不支持的平台不应允许触发注册任务"
        );
    }

    /// 验证 macOS app bundle 路径识别规则，避免 cargo run 被误判为可注册。
    #[test]
    fn macos_app_bundle_path_detection() {
        #[cfg(target_os = "macos")]
        {
            let exe = PathBuf::from("/Applications/Argus.app/Contents/MacOS/argus");
            assert_eq!(
                platform_impl::app_bundle_from_exe_path(&exe),
                Some(PathBuf::from("/Applications/Argus.app"))
            );
            let cargo_exe = PathBuf::from("/tmp/argus/target/debug/argus");
            assert_eq!(platform_impl::app_bundle_from_exe_path(&cargo_exe), None);
        }
    }

    /// 验证 Windows command 字符串保留 `%1` 引号，支持空格路径。
    #[test]
    fn windows_command_string_quotes_argument() {
        #[cfg(windows)]
        {
            let exe = PathBuf::from(r"C:\Program Files\Argus\argus.exe");
            assert_eq!(
                platform_impl::command_for_exe(&exe),
                r#""C:\Program Files\Argus\argus.exe" "%1""#
            );
        }
    }

    /// 验证 macOS 打包模板 Info.plist 显式声明了 .log 文件的具体 UTI，避免回归导致
    /// .log 文件无法出现在 Finder“打开方式”中。
    #[test]
    fn macos_info_plist_declares_log_document_type() {
        #[cfg(target_os = "macos")]
        {
            let plist_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("resources")
                .join("macos")
                .join("Info.plist");
            let plist = std::fs::read_to_string(&plist_path)
                .unwrap_or_else(|error| panic!("无法读取 {}: {error}", plist_path.display()));
            assert!(
                plist.contains("com.apple.log"),
                "Info.plist 必须声明 com.apple.log，否则 .log 文件无法右键用 Argus 打开"
            );
        }
    }
}
