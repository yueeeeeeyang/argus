//! 文件职责：统一管理需要保密的连接凭据，并优先把它们保存到系统凭据库。
//! 创建日期：2026-10-05
//! 修改日期：2026-10-05
//! 作者：Argus 开发团队
//! 主要功能：以系统凭据库为首选后端保存连接密码与私钥口令，凭据库不可用时改用机器绑定
//! 派生密钥加密后写入设置文件；任何情况下都不再把机密明文写入 `settings.toml`。

use std::io::Write;
use std::path::Path;

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::array::Array;
use aes_gcm::aead::{Aead, KeyInit};
use hkdf::Hkdf;
use secrecy::SecretString;
use sha2::Sha256;

use crate::config::paths::argus_secret_salt_file;

/// 连接凭据在系统凭据库中的固定服务名。
///
/// 测试构建刻意不访问真实凭据库（见 `keyring_entry`），因此该常量在测试中未被引用。
#[cfg_attr(test, allow(dead_code))]
const CONNECTION_KEYRING_SERVICE: &str = "argus.connection";
/// 本地密文版本前缀；算法升级时据此识别并迁移旧密文。
const LOCAL_CIPHERTEXT_PREFIX: &str = "v1:";
/// AES-256-GCM 密钥长度。
const KEY_BYTES: usize = 32;
/// AES-256-GCM 推荐 nonce 长度。
const NONCE_BYTES: usize = 12;
/// HKDF 用途标识，避免同一份密钥材料被复用到其他场景。
const HKDF_INFO: &[u8] = b"argus.local-connection-secret.v1";
/// 随机盐长度。
const SALT_BYTES: usize = 32;

/// 一次机密写入实际落到的后端。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SecretWriteOutcome {
    /// 已写入系统凭据库，设置文件中不需要保存任何密文。
    Keyring,
    /// 系统凭据库不可用，返回需要随设置文件保存的本地密文。
    LocalCiphertext(String),
}

/// 生成一条连接机密的账户名。
///
/// 参数说明：
/// - `link_id`：连接节点稳定 ID。
/// - `slot`：机密槽位标识，例如 `ssh_password`。
///
/// 返回值：不含任何机密文本的账户名，用于凭据库条目与本地密文表的键。
pub(crate) fn account_for_link(link_id: usize, slot: &str) -> String {
    format!("link/{link_id}/{slot}")
}

/// 读取一条连接机密。
///
/// 参数说明：
/// - `account`：由 [`account_for_link`] 生成的账户名。
/// - `local_ciphertext`：设置文件中保存的本地密文，凭据库可用时通常为空。
///
/// 返回值：读取成功返回机密；两处都不存在时返回 `None`；凭据库与密文都不可用时返回错误。
pub(crate) fn load_secret(
    account: &str,
    local_ciphertext: Option<&str>,
) -> Result<Option<SecretString>, String> {
    match keyring_entry(account) {
        Ok(entry) => match entry.get_password() {
            Ok(value) => return Ok(Some(SecretString::from(value))),
            // 凭据库中没有该条目时才查看本地密文，避免掩盖凭据库的真实故障。
            Err(keyring::Error::NoEntry) => {}
            Err(error) => {
                return Err(format!("无法从系统凭据库读取连接凭据：{error}"));
            }
        },
        Err(_) => {
            // 凭据库整体不可用（例如 Linux 缺少 Secret Service）：改读本地密文。
        }
    }

    match local_ciphertext {
        Some(ciphertext) => decrypt_local(ciphertext).map(|value| Some(SecretString::from(value))),
        None => Ok(None),
    }
}

/// 保存一条连接机密。
///
/// 参数说明：
/// - `account`：由 [`account_for_link`] 生成的账户名。
/// - `value`：机密明文，仅在当前调用栈与目标后端中短暂存在。
///
/// 返回值：说明机密最终落到凭据库还是本地密文；两者都失败时返回错误，绝不静默丢弃机密。
pub(crate) fn store_secret(account: &str, value: &str) -> Result<SecretWriteOutcome, String> {
    match keyring_entry(account) {
        Ok(entry) => match entry.set_password(value) {
            Ok(()) => Ok(SecretWriteOutcome::Keyring),
            Err(error) => fall_back_to_local(value, &format!("系统凭据库写入失败：{error}")),
        },
        Err(error) => fall_back_to_local(value, &error),
    }
}

/// 删除一条连接机密；不存在时视为成功，保证删除操作幂等。
pub(crate) fn forget_secret(account: &str) -> Result<(), String> {
    let Ok(entry) = keyring_entry(account) else {
        return Ok(());
    };
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(format!("无法从系统凭据库删除连接凭据：{error}")),
    }
}

/// 在凭据库不可用时改用机器绑定密钥加密。
fn fall_back_to_local(value: &str, reason: &str) -> Result<SecretWriteOutcome, String> {
    let ciphertext =
        encrypt_local(value).map_err(|error| format!("{reason}；本地加密回落同样失败：{error}"))?;
    Ok(SecretWriteOutcome::LocalCiphertext(ciphertext))
}

/// 使用机器绑定密钥加密明文，输出带版本前缀的密文。
fn encrypt_local(plaintext: &str) -> Result<String, String> {
    let key = local_encryption_key()?;
    let cipher = Aes256Gcm::new(&Array::from(key));
    let mut nonce_bytes = [0_u8; NONCE_BYTES];
    getrandom::getrandom(&mut nonce_bytes)
        .map_err(|error| format!("无法生成本地加密随机数：{error}"))?;

    let ciphertext = cipher
        .encrypt(&Array::from(nonce_bytes), plaintext.as_bytes())
        .map_err(|_| "本地凭据加密失败".to_string())?;

    let mut payload = Vec::with_capacity(NONCE_BYTES + ciphertext.len());
    payload.extend_from_slice(&nonce_bytes);
    payload.extend_from_slice(&ciphertext);
    Ok(format!(
        "{LOCAL_CIPHERTEXT_PREFIX}{}",
        base64_encode(&payload)
    ))
}

/// 解密本地密文；版本前缀、长度或认证标签不匹配时返回错误。
fn decrypt_local(ciphertext: &str) -> Result<String, String> {
    let encoded = ciphertext
        .strip_prefix(LOCAL_CIPHERTEXT_PREFIX)
        .ok_or_else(|| "本地连接凭据版本无法识别，请重新输入该连接的密码".to_string())?;
    let payload = base64_decode(encoded)?;
    if payload.len() <= NONCE_BYTES {
        return Err("本地连接凭据内容不完整，请重新输入该连接的密码".to_string());
    }

    // 复制到定长数组后再解密：nonce 长度是算法常量，长度不足已在上面拦截。
    let mut nonce_bytes = [0_u8; NONCE_BYTES];
    nonce_bytes.copy_from_slice(&payload[..NONCE_BYTES]);
    let key = local_encryption_key()?;
    let cipher = Aes256Gcm::new(&Array::from(key));
    let plaintext = cipher
        .decrypt(&Array::from(nonce_bytes), &payload[NONCE_BYTES..])
        // 认证失败通常意味着设置文件被复制到了其他机器或已被篡改，两种原因都需要用户重新输入。
        .map_err(|_| {
            "本地连接凭据无法解密，可能是设置文件来自其他机器，请重新输入密码".to_string()
        })?;

    String::from_utf8(plaintext)
        .map_err(|_| "本地连接凭据解码结果不是合法文本，请重新输入该连接的密码".to_string())
}

/// 派生本地回落加密密钥。
///
/// 密钥 = HKDF(机器标识, 随机盐)。机器标识保证密文不能跨机器解密，随机盐由本机独占
/// 保存（0600），使同一台机器上的其他账户也无法推导出密钥；代码中不保存任何密钥常量。
fn local_encryption_key() -> Result<[u8; KEY_BYTES], String> {
    let identity = machine_identity()?;
    let salt = read_or_create_salt(&argus_secret_salt_file())?;
    let hkdf = Hkdf::<Sha256>::new(Some(&salt), &identity);
    let mut key = [0_u8; KEY_BYTES];
    hkdf.expand(HKDF_INFO, &mut key)
        .map_err(|_| "派生本地连接凭据密钥失败".to_string())?;
    Ok(key)
}

/// 读取随机盐；不存在时生成并以 0600 权限写入。
///
/// 并发语义：盐必须全进程唯一——否则同一份密文会被不同密钥加密后互相无法解密。因此：
/// 1. 创建走独占的 `create_new`，只有一个调用者能成为创建者；
/// 2. 其余调用者**只等待并复用**同一份盐，绝不因为读到半截文件就删除重建（那会换掉密钥，
///    让创建者刚加密的内容立即无法解密）；
/// 3. 只有在等待预算耗尽、内容仍不完整时才判定为崩溃残留并清理重建。
fn read_or_create_salt(path: &Path) -> Result<Vec<u8>, String> {
    /// 等待创建者写完 32 字节盐的重试次数上限。
    const READ_RETRY_LIMIT: usize = 50;
    /// 每次重试之间的等待时间；总预算 100ms，远小于任何用户可感知的延迟。
    const READ_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(2);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("无法创建本地凭据目录 {}：{error}", parent.display()))?;
    }

    let mut salt = vec![0_u8; SALT_BYTES];
    getrandom::getrandom(&mut salt).map_err(|error| format!("无法生成本地凭据随机盐：{error}"))?;

    // 阶段一：尝试成为唯一创建者。
    match create_salt_file(path, &salt) {
        Ok(()) => return Ok(salt),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!(
                "无法写入本地凭据随机盐 {}：{error}",
                path.display()
            ));
        }
    }

    // 阶段二：已存在——等待创建者写完并复用同一份盐。
    for _ in 0..READ_RETRY_LIMIT {
        match std::fs::read(path) {
            Ok(existing) if existing.len() == SALT_BYTES => return Ok(existing),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "无法读取本地凭据随机盐 {}：{error}",
                    path.display()
                ));
            }
        }
        std::thread::sleep(READ_RETRY_INTERVAL);
    }

    // 阶段三：等待预算耗尽仍不完整，判定为崩溃残留并重建。
    std::fs::remove_file(path)
        .map_err(|error| format!("无法清理损坏的本地凭据随机盐 {}：{error}", path.display()))?;
    create_salt_file(path, &salt)
        .map_err(|error| format!("无法重建本地凭据随机盐 {}：{error}", path.display()))?;
    Ok(salt)
}

/// 以独占且仅所有者可读写的方式创建盐文件。
///
/// 返回值：创建成功返回 `Ok`；文件已存在返回 `AlreadyExists`，由调用方按并发语义处理。
fn create_salt_file(path: &Path, salt: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        // 仅依赖进程 umask 不足以保证其他账户不可读该盐。
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(salt)?;
    Ok(())
}

/// 返回当前机器的稳定标识。
///
/// 返回值：读取成功返回机器标识字节；平台不支持或读取失败时返回错误，调用方据此明确
/// 失败，而不是退化成所有机器共用的常量密钥。
fn machine_identity() -> Result<Vec<u8>, String> {
    #[cfg(target_os = "linux")]
    {
        for candidate in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
            if let Ok(text) = std::fs::read_to_string(candidate) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Ok(trimmed.as_bytes().to_vec());
                }
            }
        }
        Err("无法读取本机 machine-id，本地凭据加密不可用".to_string())
    }

    #[cfg(target_os = "windows")]
    {
        windows_machine_guid()
    }

    #[cfg(target_os = "macos")]
    {
        macos_platform_uuid()
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        Err("当前平台不支持机器绑定密钥派生".to_string())
    }
}

/// 从 Windows 注册表读取 `MachineGuid` 作为机器标识。
#[cfg(target_os = "windows")]
fn windows_machine_guid() -> Result<Vec<u8>, String> {
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
    };

    /// 读取注册表字符串值所需的缓冲区字节数；MachineGuid 固定为 36 个字符加结尾空字符。
    const BUFFER_BYTES: usize = 128;

    let sub_key: Vec<u16> = "SOFTWARE\\Microsoft\\Cryptography\0"
        .encode_utf16()
        .collect();
    let value_name: Vec<u16> = "MachineGuid\0".encode_utf16().collect();
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: 只读取注册表字符串值，句柄在使用后立即关闭。
    unsafe {
        let status = RegOpenKeyExW(HKEY_LOCAL_MACHINE, sub_key.as_ptr(), 0, KEY_READ, &mut key);
        if status != 0 {
            return Err(format!(
                "无法打开 Windows 注册表 MachineGuid：错误码 {status}"
            ));
        }

        let mut buffer = [0_u8; BUFFER_BYTES];
        let mut buffer_len = BUFFER_BYTES as u32;
        let mut value_type = 0_u32;
        let status = RegQueryValueExW(
            key,
            value_name.as_ptr(),
            std::ptr::null_mut(),
            &mut value_type,
            buffer.as_mut_ptr(),
            &mut buffer_len,
        );
        RegCloseKey(key);
        if status != 0 {
            return Err(format!(
                "无法读取 Windows 注册表 MachineGuid：错误码 {status}"
            ));
        }
        if value_type != REG_SZ {
            return Err("Windows MachineGuid 类型不是字符串".to_string());
        }

        let units = buffer[..buffer_len as usize]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|unit| *unit != 0)
            .collect::<Vec<_>>();
        let text = String::from_utf16_lossy(&units);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err("Windows MachineGuid 为空".to_string());
        }
        Ok(trimmed.as_bytes().to_vec())
    }
}

/// 通过 IOKit 读取 macOS 平台 UUID 作为机器标识。
#[cfg(target_os = "macos")]
fn macos_platform_uuid() -> Result<Vec<u8>, String> {
    use std::ffi::{CStr, CString, c_char, c_void};

    /// CoreFoundation 的 UTF-8 字符串编码常量。
    const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    /// `kIOMainPortDefault`：传入 0 表示使用默认主端口。
    const IO_MAIN_PORT_DEFAULT: u32 = 0;
    /// 平台 UUID 字符串长度上限（36 个字符加结尾空字符）。
    const UUID_BUFFER_BYTES: usize = 64;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        fn IOServiceGetMatchingService(main_port: u32, matching: *mut c_void) -> u32;
        fn IORegistryEntryCreateCFProperty(
            entry: u32,
            key: *const c_void,
            allocator: *const c_void,
            options: u32,
        ) -> *const c_void;
        fn IOObjectRelease(object: u32) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            c_str: *const c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFStringGetCString(
            the_string: *const c_void,
            buffer: *mut c_char,
            buffer_size: isize,
            encoding: u32,
        ) -> u8;
        fn CFRelease(cf: *const c_void);
    }

    // SAFETY: 只读取 IOKit 注册表中的只读属性；每个 CoreFoundation 对象与 IOKit 句柄都在
    // 使用后释放，提前返回的分支也不会泄漏句柄。
    unsafe {
        let service_name = CString::new("IOPlatformExpertDevice")
            .map_err(|_| "无法构造 IOKit 服务名".to_string())?;
        let matching = IOServiceMatching(service_name.as_ptr());
        if matching.is_null() {
            return Err("无法构造 IOKit 匹配字典".to_string());
        }
        let service = IOServiceGetMatchingService(IO_MAIN_PORT_DEFAULT, matching);
        if service == 0 {
            return Err("无法获取 IOKit 平台专家设备".to_string());
        }

        let key_name =
            CString::new("IOPlatformUUID").map_err(|_| "无法构造 IOKit 属性名".to_string())?;
        let key =
            CFStringCreateWithCString(std::ptr::null(), key_name.as_ptr(), CF_STRING_ENCODING_UTF8);
        if key.is_null() {
            IOObjectRelease(service);
            return Err("无法构造 IOKit 属性键".to_string());
        }

        let value = IORegistryEntryCreateCFProperty(service, key, std::ptr::null(), 0);
        CFRelease(key);
        IOObjectRelease(service);
        if value.is_null() {
            return Err("无法读取 IOKit IOPlatformUUID".to_string());
        }

        let mut buffer = [0 as c_char; UUID_BUFFER_BYTES];
        let converted = CFStringGetCString(
            value,
            buffer.as_mut_ptr(),
            UUID_BUFFER_BYTES as isize,
            CF_STRING_ENCODING_UTF8,
        );
        CFRelease(value);
        if converted == 0 {
            return Err("无法转换 IOKit IOPlatformUUID 文本".to_string());
        }

        let text = CStr::from_ptr(buffer.as_ptr()).to_string_lossy();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err("IOKit IOPlatformUUID 为空".to_string());
        }
        Ok(trimmed.as_bytes().to_vec())
    }
}

/// 返回凭据库条目；统一在此处收敛不可用的后端错误文本。
#[cfg(not(test))]
fn keyring_entry(account: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(CONNECTION_KEYRING_SERVICE, account)
        .map_err(|error| format!("无法访问系统凭据库：{error}"))
}

/// 测试构建固定返回“凭据库不可用”。
///
/// 说明：单元测试绝不能读写用户真实的系统钥匙串——那既会污染用户凭据，也让结果依赖运行
/// 环境。测试因此统一走本地加密回落路径，该路径正是生产环境在无凭据库时的真实分支。
#[cfg(test)]
fn keyring_entry(_account: &str) -> Result<keyring::Entry, String> {
    Err("测试构建不访问系统凭据库".to_string())
}

/// 使用标准字母表编码字节。
fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// 解码标准 base64；失败时返回面向用户的错误。
fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|_| "本地连接凭据编码无效，请重新输入该连接的密码".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret as _;

    /// 验证账户名只包含连接 ID 与槽位，不泄漏任何机密文本。
    #[test]
    fn account_names_do_not_expose_secrets() {
        assert_eq!(account_for_link(7, "ssh_password"), "link/7/ssh_password");
    }

    /// 验证本地密文可以往返解密，且同一明文的两次加密结果不同（nonce 随机）。
    #[test]
    fn local_ciphertext_round_trips_with_random_nonce() {
        let first = encrypt_local("s3cret-密码").expect("加密应成功");
        let second = encrypt_local("s3cret-密码").expect("加密应成功");

        assert!(first.starts_with(LOCAL_CIPHERTEXT_PREFIX));
        assert_ne!(first, second, "随机 nonce 必须让密文不同");
        assert_eq!(decrypt_local(&first).expect("解密应成功"), "s3cret-密码");
        assert!(!first.contains("s3cret"), "密文不得包含明文片段");
    }

    /// 验证凭据库不可用时落盘的是本地密文而不是明文。
    #[test]
    fn store_secret_falls_back_to_ciphertext_without_plaintext() {
        let account = account_for_link(1, "smb_password");
        let outcome = store_secret(&account, "plain-text-secret").expect("应回落到本地密文");
        let SecretWriteOutcome::LocalCiphertext(ciphertext) = outcome else {
            panic!("测试构建不访问系统凭据库，必须返回本地密文");
        };

        assert!(!ciphertext.contains("plain-text-secret"));
        let loaded = load_secret(&account, Some(&ciphertext))
            .expect("应能读回")
            .expect("应存在机密");
        assert_eq!(loaded.expose_secret(), "plain-text-secret");
    }

    /// 验证既没有凭据库条目也没有本地密文时返回 `None`，而不是报错或返回空串。
    #[test]
    fn load_secret_without_any_source_returns_none() {
        let account = account_for_link(99, "svn_password");
        assert!(load_secret(&account, None).expect("应无错误").is_none());
    }

    /// 验证损坏密文会被拒绝，并给出可操作的提示。
    #[test]
    fn corrupted_ciphertext_is_rejected() {
        let error = decrypt_local("v1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .expect_err("伪造密文必须解密失败");
        assert!(
            error.contains("重新输入"),
            "错误提示应指导用户恢复：{error}"
        );
        assert!(decrypt_local("no-version-prefix").is_err());
    }

    /// 验证随机盐首次使用会生成并复用同一个 0600 文件。
    #[test]
    fn salt_is_created_once_and_reused() {
        let directory = crate::config::paths::isolated_test_dir("secret-salt");
        std::fs::create_dir_all(&directory).expect("应能创建测试目录");
        let path = directory.join("secret.salt");

        let first = read_or_create_salt(&path).expect("应能生成盐");
        let second = read_or_create_salt(&path).expect("应能复用盐");
        assert_eq!(first, second);
        assert_eq!(first.len(), SALT_BYTES);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path)
                .expect("应能读取盐文件元信息")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "盐文件必须仅所有者可读写");
        }
    }
}
