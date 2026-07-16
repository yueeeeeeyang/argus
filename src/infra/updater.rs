//! 文件职责：实现 Argus 自动升级核心流程。
//! 创建日期：2026-06-16
//! 修改日期：2026-07-16
//! 作者：Argus 开发团队
//! 主要功能：拉取升级清单、校验 Ed25519 签名、选择平台资源、下载校验安装包，并替换当前二进制或 macOS `.app`。

use std::ffi::OsString;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;
use walkdir::WalkDir;
use zip::ZipArchive;

use crate::config::UpgradeConfig;
use crate::config::paths::argus_updates_dir;

/// 升级清单文件名。
const MANIFEST_FILE_NAME: &str = "manifest-v1.json";
/// 升级清单签名文件名。
const MANIFEST_SIGNATURE_FILE_NAME: &str = "manifest-v1.json.sig";
/// 下载中的安装包后缀，避免半文件被误当成可安装文件。
const DOWNLOADED_BINARY_EXTENSION: &str = "download";
/// macOS `.app` 升级包下载后保存的文件名。
const MACOS_APP_ARCHIVE_FILE_NAME: &str = "Argus.app.zip.download";
/// macOS `.app` 升级包解压目录名。
const MACOS_APP_EXTRACTED_DIR_NAME: &str = "app";
/// 升级 HTTP 建连超时时间，避免网络黑洞让检查任务永久挂起。
const UPGRADE_HTTP_CONNECT_TIMEOUT_SECONDS: u64 = 10;
/// 升级 HTTP 读取超时时间，避免服务器停止输出后安装状态一直忙碌。
const UPGRADE_HTTP_READ_TIMEOUT_SECONDS: u64 = 60;
/// 升级 HTTP 写入超时时间，覆盖 TLS/代理等少量需要写请求体元数据的场景。
const UPGRADE_HTTP_WRITE_TIMEOUT_SECONDS: u64 = 30;
/// 单次升级 HTTP 请求总超时时间，下载较大安装包时仍给出有限等待窗口。
const UPGRADE_HTTP_TOTAL_TIMEOUT_SECONDS: u64 = 300;

/// 升级流程错误，UI 层会把它转换为用户可读提示。
#[derive(Debug, Error)]
pub(crate) enum UpgradeError {
    /// 未配置升级服务器地址。
    #[error("未配置升级服务器地址")]
    MissingServerUrl,
    /// 未配置升级清单验签公钥。
    #[error("未配置升级清单验签公钥")]
    MissingPublicKey,
    /// 升级服务器地址无法解析。
    #[error("升级服务器地址无效：{0}")]
    InvalidServerUrl(String),
    /// 网络请求失败。
    #[error("升级服务器请求失败：{0}")]
    Network(String),
    /// 清单签名格式错误。
    #[error("升级清单签名格式错误：{0}")]
    InvalidSignature(String),
    /// 清单签名校验失败。
    #[error("升级清单签名校验失败：{0}")]
    SignatureVerification(String),
    /// 清单 JSON 解析失败。
    #[error("升级清单解析失败：{0}")]
    ManifestParse(#[from] serde_json::Error),
    /// 当前版本或远端版本号不是合法 SemVer。
    #[error("版本号格式无效：{0}")]
    InvalidVersion(String),
    /// 清单中没有当前平台可用的升级资源。
    #[error("升级清单未提供当前平台资源：{0}/{1}")]
    MissingPlatformAsset(String, String),
    /// 当前运行环境无法安全执行自动升级。
    #[error("当前运行环境不支持自动升级：{0}")]
    UnsupportedInstallTarget(String),
    /// 下载文件大小与清单不一致。
    #[error("升级包大小校验失败：期望 {expected} 字节，实际 {actual} 字节")]
    SizeMismatch {
        /// 清单声明的字节数。
        expected: u64,
        /// 实际下载的字节数。
        actual: u64,
    },
    /// 下载文件 SHA-256 与清单不一致。
    #[error("升级包 SHA-256 校验失败")]
    HashMismatch,
    /// 安装包结构不符合当前平台要求。
    #[error("升级包格式错误：{0}")]
    InvalidPackage(String),
    /// 文件系统读写失败。
    #[error("升级文件处理失败：{0}")]
    Io(#[from] std::io::Error),
    /// 替换当前二进制失败。
    #[error("替换当前程序失败：{0}")]
    Replace(String),
    /// 重启新进程失败。
    #[error("重启 Argus 失败：{0}")]
    Restart(String),
}

/// 升级检查结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum UpgradeCheckOutcome {
    /// 未启用或缺少服务器配置，因此没有发起检查。
    Disabled,
    /// 当前已经是最新版本。
    UpToDate,
    /// 发现的新版本已被用户选择跳过。
    Skipped(String),
    /// 找到了可安装的新版本。
    Available(AvailableUpgrade),
}

/// 可安装升级信息，供 UI 展示和下载流程复用。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AvailableUpgrade {
    /// 新版本号。
    pub version: String,
    /// 升级日志，直接来自 manifest。
    pub release_notes: String,
    /// 发布时间，直接来自 manifest。
    pub published_at: String,
    /// 当前平台对应的二进制资源。
    pub asset: UpgradeAsset,
}

/// 已下载并完成校验的升级安装包。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreparedUpgrade {
    /// 新版本号。
    pub version: String,
    /// 可交给安装流程使用的本地替换路径；可能是二进制文件，也可能是 `.app` 目录。
    pub replacement_path: PathBuf,
    /// 当前安装目标类型，决定最终替换和重启方式。
    pub install_target: PreparedInstallTarget,
}

/// 已准备好的安装目标类型。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PreparedInstallTarget {
    /// 替换当前可执行文件，适用于 Windows/Linux 裸二进制运行场景。
    CurrentBinary,
    /// 替换当前 macOS `.app` bundle，适用于正式 `.app` 分发场景。
    MacAppBundle {
        /// 当前正在运行的 `.app` bundle 路径，例如 `/Applications/Argus.app`。
        current_bundle: PathBuf,
    },
}

/// 当前运行环境对应的安装目标。
#[derive(Clone, Debug, Eq, PartialEq)]
enum InstallTarget {
    /// 裸二进制替换。
    CurrentBinary,
    /// macOS `.app` bundle 替换。
    MacAppBundle {
        /// 当前 `.app` bundle 路径。
        current_bundle: PathBuf,
        /// 当前 bundle 内主可执行文件名，用于校验新 bundle 结构。
        executable_name: OsString,
    },
}

/// manifest v1 根对象。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct UpgradeManifest {
    /// 新版本号，必须是 SemVer 或带 `v` 前缀的 SemVer。
    pub version: String,
    /// 升级日志文本。
    pub release_notes: String,
    /// 发布时间，推荐 RFC3339 字符串。
    pub published_at: String,
    /// 各平台二进制资源列表。
    pub assets: Vec<UpgradeAsset>,
}

/// manifest v1 中的平台资源对象。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct UpgradeAsset {
    /// 目标操作系统，例如 `macos` 或 `windows`。
    pub os: String,
    /// 目标 CPU 架构，例如 `aarch64` 或 `x86_64`。
    pub arch: String,
    /// 下载地址，可为绝对 URL，也可相对升级服务器根地址。
    pub url: String,
    /// 二进制文件 SHA-256 十六进制摘要。
    pub sha256: String,
    /// 二进制文件大小，单位字节。
    pub size_bytes: u64,
}

/// 升级网络客户端抽象，测试可注入内存实现避免真实网络访问。
pub(crate) trait UpgradeHttpClient: Clone + Send + Sync + 'static {
    /// 下载指定 URL 的完整响应体。
    ///
    /// 参数说明：
    /// - `url`：绝对 HTTP/HTTPS 地址。
    ///
    /// 返回值：响应体字节；网络错误或非成功状态码应返回错误。
    fn get_bytes(&self, url: &str) -> Result<Vec<u8>, UpgradeError>;
}

/// manifest 验签抽象，测试可注入固定密钥验证不同签名场景。
pub(crate) trait ManifestVerifier: Clone + Send + Sync + 'static {
    /// 校验 manifest 原文与签名是否匹配。
    fn verify(&self, manifest_bytes: &[u8], signature_bytes: &[u8]) -> Result<(), UpgradeError>;
}

/// 安装替换和重启抽象，测试中禁止真实替换当前测试进程。
pub(crate) trait BinaryReplacer: Clone + Send + Sync + 'static {
    /// 使用指定二进制替换当前运行的程序。
    fn replace_current_binary(&self, replacement_path: &Path) -> Result<(), UpgradeError>;

    /// 使用指定 `.app` bundle 替换当前运行的 macOS 应用包。
    fn replace_current_app_bundle(
        &self,
        current_bundle: &Path,
        replacement_bundle: &Path,
    ) -> Result<(), UpgradeError> {
        let _ = (current_bundle, replacement_bundle);
        Err(UpgradeError::Replace(
            "当前替换器不支持 macOS .app bundle 替换".to_string(),
        ))
    }

    /// 启动替换后的当前程序路径。
    fn restart_current_binary(&self, current_exe: &Path) -> Result<(), UpgradeError>;

    /// 通过 LaunchServices 启动替换后的 macOS `.app` bundle。
    fn restart_app_bundle(&self, app_bundle: &Path) -> Result<(), UpgradeError> {
        let _ = app_bundle;
        Err(UpgradeError::Restart(
            "当前替换器不支持 macOS .app bundle 重启".to_string(),
        ))
    }
}

/// 基于 `ureq` 的同步网络客户端，调用方应把它放到 GPUI 后台线程执行。
#[derive(Clone, Debug, Default)]
pub(crate) struct UreqUpgradeHttpClient;

impl UpgradeHttpClient for UreqUpgradeHttpClient {
    /// 使用阻塞 HTTP 请求下载响应体。
    fn get_bytes(&self, url: &str) -> Result<Vec<u8>, UpgradeError> {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(UPGRADE_HTTP_CONNECT_TIMEOUT_SECONDS))
            .timeout_read(Duration::from_secs(UPGRADE_HTTP_READ_TIMEOUT_SECONDS))
            .timeout_write(Duration::from_secs(UPGRADE_HTTP_WRITE_TIMEOUT_SECONDS))
            .timeout(Duration::from_secs(UPGRADE_HTTP_TOTAL_TIMEOUT_SECONDS))
            .build();
        let response = agent
            .get(url)
            .call()
            .map_err(|error| UpgradeError::Network(error.to_string()))?;
        let mut reader = response.into_reader();
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .map_err(|error| UpgradeError::Network(error.to_string()))?;
        Ok(bytes)
    }
}

/// Ed25519 manifest 验签器。
#[derive(Clone, Debug)]
pub(crate) struct Ed25519ManifestVerifier {
    /// Base64 编码的 32 字节 Ed25519 公钥。
    public_key_base64: String,
}

impl Ed25519ManifestVerifier {
    /// 使用配置中的 Ed25519 公钥构造验签器。
    ///
    /// 参数说明：
    /// - `public_key_base64`：32 字节 Ed25519 公钥的 Base64 文本。
    ///
    /// 返回值：后续用于 manifest 签名校验的验签器。
    pub(crate) fn from_public_key(public_key_base64: String) -> Self {
        Self { public_key_base64 }
    }
}

impl ManifestVerifier for Ed25519ManifestVerifier {
    /// 校验 manifest 原文字节的 Ed25519 签名。
    fn verify(&self, manifest_bytes: &[u8], signature_bytes: &[u8]) -> Result<(), UpgradeError> {
        let public_key = decode_base64_fixed::<32>(&self.public_key_base64)?;
        let signature =
            decode_base64_fixed::<64>(std::str::from_utf8(signature_bytes).map_err(|error| {
                UpgradeError::InvalidSignature(format!("签名文件不是 UTF-8 文本：{error}"))
            })?)?;
        let verifying_key = VerifyingKey::from_bytes(&public_key)
            .map_err(|error| UpgradeError::InvalidSignature(error.to_string()))?;
        let signature = Signature::from_bytes(&signature);

        verifying_key
            .verify(manifest_bytes, &signature)
            .map_err(|error| UpgradeError::SignatureVerification(error.to_string()))
    }
}

/// 基于 self-replace 和目录交换的真实安装替换器。
#[derive(Clone, Debug, Default)]
pub(crate) struct SelfReplaceBinaryReplacer;

impl BinaryReplacer for SelfReplaceBinaryReplacer {
    /// 调用 `self_replace` 完成当前可执行文件替换。
    fn replace_current_binary(&self, replacement_path: &Path) -> Result<(), UpgradeError> {
        self_replace::self_replace(replacement_path)
            .map_err(|error| UpgradeError::Replace(error.to_string()))
    }

    /// 替换当前 macOS `.app` bundle。
    ///
    /// 说明：下载目录通常在 `~/.argus`，而应用可能位于 `/Applications`。这里先把新
    /// bundle 复制到当前 `.app` 的同级临时目录，再通过同目录 rename 完成替换，避免
    /// 跨卷 rename 失败；旧 bundle 会移动到同级备份路径，便于失败时回滚。
    fn replace_current_app_bundle(
        &self,
        current_bundle: &Path,
        replacement_bundle: &Path,
    ) -> Result<(), UpgradeError> {
        if !cfg!(target_os = "macos") {
            return Err(UpgradeError::Replace(
                "当前平台不支持 .app bundle 替换".to_string(),
            ));
        }
        let parent = current_bundle.parent().ok_or_else(|| {
            UpgradeError::Replace(format!("无法定位 {} 的父目录", current_bundle.display()))
        })?;
        let staging_bundle = app_bundle_sibling_path(current_bundle, "argus-new")?;
        let backup_bundle = app_bundle_sibling_path(current_bundle, "argus-backup")?;

        remove_dir_if_exists(&staging_bundle)?;
        copy_dir_all(replacement_bundle, &staging_bundle)?;
        if !staging_bundle.starts_with(parent) {
            return Err(UpgradeError::Replace(
                "新 .app 临时目录位置异常".to_string(),
            ));
        }

        remove_dir_if_exists(&backup_bundle)?;
        fs::rename(current_bundle, &backup_bundle).map_err(|error| {
            UpgradeError::Replace(format!(
                "备份当前 .app 失败：{} -> {}：{error}",
                current_bundle.display(),
                backup_bundle.display()
            ))
        })?;

        match fs::rename(&staging_bundle, current_bundle) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = fs::rename(&backup_bundle, current_bundle);
                Err(UpgradeError::Replace(format!(
                    "替换 .app 失败，已尝试回滚：{} -> {}：{error}",
                    staging_bundle.display(),
                    current_bundle.display()
                )))
            }
        }
    }

    /// 通过当前可执行文件路径拉起替换后的新进程。
    fn restart_current_binary(&self, current_exe: &Path) -> Result<(), UpgradeError> {
        Command::new(current_exe)
            .spawn()
            .map_err(|error| UpgradeError::Restart(error.to_string()))?;
        Ok(())
    }

    /// 使用 macOS `open` 启动替换后的 `.app`。
    fn restart_app_bundle(&self, app_bundle: &Path) -> Result<(), UpgradeError> {
        if !cfg!(target_os = "macos") {
            return Err(UpgradeError::Restart(
                "当前平台不支持 .app bundle 重启".to_string(),
            ));
        }
        Command::new("open")
            .arg("-n")
            .arg(app_bundle)
            .spawn()
            .map_err(|error| UpgradeError::Restart(error.to_string()))?;
        Ok(())
    }
}

/// 自动升级服务，组合网络、验签和二进制替换能力。
#[derive(Clone, Debug)]
pub(crate) struct UpgradeService<C, V, R> {
    /// 升级网络客户端。
    http_client: C,
    /// manifest 验签器。
    verifier: V,
    /// 当前二进制替换器。
    replacer: R,
    /// 升级文件缓存根目录。
    updates_dir: PathBuf,
}

impl UpgradeService<UreqUpgradeHttpClient, Ed25519ManifestVerifier, SelfReplaceBinaryReplacer> {
    /// 构造生产环境使用的升级服务。
    ///
    /// 参数说明：
    /// - `config`：升级配置，提供 manifest 验签公钥。
    ///
    /// 返回值：使用真实网络、真实验签和真实安装替换器的升级服务。
    pub(crate) fn runtime(config: &UpgradeConfig) -> Self {
        Self {
            http_client: UreqUpgradeHttpClient,
            verifier: Ed25519ManifestVerifier::from_public_key(config.public_key_base64.clone()),
            replacer: SelfReplaceBinaryReplacer,
            updates_dir: argus_updates_dir(),
        }
    }
}

impl<C, V, R> UpgradeService<C, V, R>
where
    C: UpgradeHttpClient,
    V: ManifestVerifier,
    R: BinaryReplacer,
{
    /// 构造可注入依赖的升级服务，主要供单元测试复用。
    #[cfg(test)]
    pub(crate) fn new(http_client: C, verifier: V, replacer: R, updates_dir: PathBuf) -> Self {
        Self {
            http_client,
            verifier,
            replacer,
            updates_dir,
        }
    }

    /// 检查升级服务器是否存在比当前版本更高的新版本。
    ///
    /// 参数说明：
    /// - `config`：升级配置，提供启用开关和服务器地址。
    /// - `current_version`：当前程序版本。
    /// - `respect_skipped_version`：自动检查应尊重跳过版本，手动检查可忽略跳过状态。
    pub(crate) fn check_for_update(
        &self,
        config: &UpgradeConfig,
        current_version: &str,
        respect_skipped_version: bool,
    ) -> Result<UpgradeCheckOutcome, UpgradeError> {
        if !config.enabled {
            return Ok(UpgradeCheckOutcome::Disabled);
        }
        if config.server_url.trim().is_empty() {
            return Ok(UpgradeCheckOutcome::Disabled);
        }
        if config.public_key_base64.trim().is_empty() {
            return Err(UpgradeError::MissingPublicKey);
        }

        let server_url = parse_server_url(&config.server_url)?;
        let manifest_url = server_url
            .join(MANIFEST_FILE_NAME)
            .map_err(|error| UpgradeError::InvalidServerUrl(error.to_string()))?;
        let signature_url = server_url
            .join(MANIFEST_SIGNATURE_FILE_NAME)
            .map_err(|error| UpgradeError::InvalidServerUrl(error.to_string()))?;
        let manifest_bytes = self.http_client.get_bytes(manifest_url.as_str())?;
        let signature_bytes = self.http_client.get_bytes(signature_url.as_str())?;

        self.verifier.verify(&manifest_bytes, &signature_bytes)?;

        let mut manifest: UpgradeManifest = serde_json::from_slice(&manifest_bytes)?;
        normalize_manifest_urls(&mut manifest, &server_url)?;
        if !is_remote_version_newer(current_version, &manifest.version)? {
            return Ok(UpgradeCheckOutcome::UpToDate);
        }

        if respect_skipped_version
            && config.skipped_version.as_deref() == Some(manifest.version.trim())
        {
            return Ok(UpgradeCheckOutcome::Skipped(manifest.version));
        }

        let os = current_platform_os();
        let arch = current_platform_arch();
        let asset = select_platform_asset(&manifest, os, arch)
            .ok_or_else(|| UpgradeError::MissingPlatformAsset(os.to_string(), arch.to_string()))?;

        Ok(UpgradeCheckOutcome::Available(AvailableUpgrade {
            version: manifest.version,
            release_notes: manifest.release_notes,
            published_at: manifest.published_at,
            asset,
        }))
    }

    /// 下载并校验升级安装包，返回可安装的本地路径。
    pub(crate) fn download_and_prepare(
        &self,
        upgrade: &AvailableUpgrade,
    ) -> Result<PreparedUpgrade, UpgradeError> {
        let install_target = current_install_target()?;
        self.download_and_prepare_for_target(upgrade, install_target)
    }

    /// 按指定安装目标下载并准备升级包；测试可注入 macOS `.app` 目标。
    fn download_and_prepare_for_target(
        &self,
        upgrade: &AvailableUpgrade,
        install_target: InstallTarget,
    ) -> Result<PreparedUpgrade, UpgradeError> {
        let package_bytes = self.http_client.get_bytes(&upgrade.asset.url)?;
        if package_bytes.len() as u64 != upgrade.asset.size_bytes {
            return Err(UpgradeError::SizeMismatch {
                expected: upgrade.asset.size_bytes,
                actual: package_bytes.len() as u64,
            });
        }

        let actual_sha256 = sha256_hex(&package_bytes);
        if !actual_sha256.eq_ignore_ascii_case(upgrade.asset.sha256.trim()) {
            return Err(UpgradeError::HashMismatch);
        }

        let version_dir = self
            .updates_dir
            .join(sanitize_version_for_path(&upgrade.version));
        fs::create_dir_all(&version_dir)?;
        match install_target {
            InstallTarget::CurrentBinary => {
                prepare_binary_replacement(upgrade, &package_bytes, &version_dir)
            }
            InstallTarget::MacAppBundle {
                current_bundle,
                executable_name,
            } => prepare_macos_app_bundle_replacement(
                upgrade,
                &package_bytes,
                &version_dir,
                current_bundle,
                &executable_name,
            ),
        }
    }

    /// 使用已准备好的升级安装包替换当前程序并拉起新进程。
    pub(crate) fn install_prepared_upgrade(
        &self,
        prepared: &PreparedUpgrade,
    ) -> Result<(), UpgradeError> {
        match &prepared.install_target {
            PreparedInstallTarget::CurrentBinary => {
                let current_exe = std::env::current_exe()?;
                self.replacer
                    .replace_current_binary(&prepared.replacement_path)?;
                self.replacer.restart_current_binary(&current_exe)
            }
            PreparedInstallTarget::MacAppBundle { current_bundle } => {
                self.replacer
                    .replace_current_app_bundle(current_bundle, &prepared.replacement_path)?;
                self.replacer.restart_app_bundle(current_bundle)
            }
        }
    }
}

/// 判断当前进程应使用哪种安装目标。
fn current_install_target() -> Result<InstallTarget, UpgradeError> {
    let current_exe = std::env::current_exe()?;
    install_target_for_exe_path(&current_exe)
}

/// 根据可执行文件路径判断安装目标；macOS 必须位于 `.app` bundle 内才能安全升级。
fn install_target_for_exe_path(current_exe: &Path) -> Result<InstallTarget, UpgradeError> {
    if cfg!(target_os = "macos") {
        let Some(current_bundle) = macos_app_bundle_from_exe_path(current_exe) else {
            return Err(UpgradeError::UnsupportedInstallTarget(
                "macOS 自动升级需要从 Argus.app 启动，裸二进制运行无法安全替换 .app 升级包"
                    .to_string(),
            ));
        };
        let executable_name = current_exe.file_name().ok_or_else(|| {
            UpgradeError::InvalidPackage(format!(
                "无法识别当前可执行文件名：{}",
                current_exe.display()
            ))
        })?;
        return Ok(InstallTarget::MacAppBundle {
            current_bundle,
            executable_name: executable_name.to_os_string(),
        });
    }

    Ok(InstallTarget::CurrentBinary)
}

/// 准备裸二进制替换文件。
fn prepare_binary_replacement(
    upgrade: &AvailableUpgrade,
    package_bytes: &[u8],
    version_dir: &Path,
) -> Result<PreparedUpgrade, UpgradeError> {
    let replacement_path = version_dir.join(format!(
        "{}.{}",
        platform_binary_name(),
        DOWNLOADED_BINARY_EXTENSION
    ));
    fs::write(&replacement_path, package_bytes)?;
    make_file_executable_if_needed(&replacement_path)?;

    Ok(PreparedUpgrade {
        version: upgrade.version.clone(),
        replacement_path,
        install_target: PreparedInstallTarget::CurrentBinary,
    })
}

/// 准备 macOS `.app` bundle 替换目录。
///
/// 参数说明：
/// - `upgrade`：升级版本信息。
/// - `package_bytes`：已经通过 SHA-256 校验的 zip 包字节。
/// - `version_dir`：当前版本的升级缓存目录。
/// - `current_bundle`：当前正在运行的 `.app` bundle。
/// - `executable_name`：当前主可执行文件名，用于确认新 bundle 可启动。
///
/// 返回值：解压出的 `.app` 目录和当前 bundle 目标路径。
fn prepare_macos_app_bundle_replacement(
    upgrade: &AvailableUpgrade,
    package_bytes: &[u8],
    version_dir: &Path,
    current_bundle: PathBuf,
    executable_name: &OsString,
) -> Result<PreparedUpgrade, UpgradeError> {
    let archive_path = version_dir.join(MACOS_APP_ARCHIVE_FILE_NAME);
    fs::write(&archive_path, package_bytes)?;

    let extract_dir = version_dir.join(MACOS_APP_EXTRACTED_DIR_NAME);
    remove_dir_if_exists(&extract_dir)?;
    fs::create_dir_all(&extract_dir)?;

    let cursor = Cursor::new(package_bytes);
    let mut archive =
        ZipArchive::new(cursor).map_err(|error| UpgradeError::InvalidPackage(error.to_string()))?;
    archive
        .extract(&extract_dir)
        .map_err(|error| UpgradeError::InvalidPackage(error.to_string()))?;

    let replacement_path = find_extracted_app_bundle(&extract_dir)?;
    ensure_app_bundle_executable(&replacement_path, executable_name)?;

    Ok(PreparedUpgrade {
        version: upgrade.version.clone(),
        replacement_path,
        install_target: PreparedInstallTarget::MacAppBundle { current_bundle },
    })
}

/// 解析升级服务器根地址，并确保后续 `join` 以目录语义拼接。
fn parse_server_url(server_url: &str) -> Result<Url, UpgradeError> {
    let mut normalized = server_url.trim().to_string();
    if normalized.is_empty() {
        return Err(UpgradeError::MissingServerUrl);
    }
    if !normalized.ends_with('/') {
        normalized.push('/');
    }
    Url::parse(&normalized).map_err(|error| UpgradeError::InvalidServerUrl(error.to_string()))
}

/// 把 manifest 中的相对资源地址归一化成绝对地址。
fn normalize_manifest_urls(
    manifest: &mut UpgradeManifest,
    server_url: &Url,
) -> Result<(), UpgradeError> {
    for asset in &mut manifest.assets {
        if Url::parse(&asset.url).is_ok() {
            continue;
        }
        asset.url = server_url
            .join(&asset.url)
            .map_err(|error| UpgradeError::InvalidServerUrl(error.to_string()))?
            .to_string();
    }
    Ok(())
}

/// 选择与当前平台完全匹配的升级资源。
pub(crate) fn select_platform_asset(
    manifest: &UpgradeManifest,
    os: &str,
    arch: &str,
) -> Option<UpgradeAsset> {
    manifest
        .assets
        .iter()
        .find(|asset| asset.os == os && asset.arch == arch)
        .cloned()
}

/// 判断远端版本是否高于当前版本。
pub(crate) fn is_remote_version_newer(
    current_version: &str,
    remote_version: &str,
) -> Result<bool, UpgradeError> {
    let current = parse_semver(current_version)?;
    let remote = parse_semver(remote_version)?;
    Ok(remote > current)
}

/// 解析 SemVer，兼容发布系统常见的 `v1.2.3` 写法。
fn parse_semver(version: &str) -> Result<Version, UpgradeError> {
    Version::parse(version.trim().trim_start_matches('v'))
        .map_err(|error| UpgradeError::InvalidVersion(format!("{version}：{error}")))
}

/// 当前目标系统名称，需和 manifest 中的 `os` 字段保持一致。
pub(crate) fn current_platform_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    }
}

/// 当前目标架构名称，需和 manifest 中的 `arch` 字段保持一致。
pub(crate) fn current_platform_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "arm") {
        "arm"
    } else {
        "unknown"
    }
}

/// 当前平台的主程序文件名。
fn platform_binary_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "argus.exe"
    } else {
        "argus"
    }
}

/// 从可执行文件路径反推出 macOS `.app` bundle 路径。
fn macos_app_bundle_from_exe_path(exe: &Path) -> Option<PathBuf> {
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

/// 判断路径是否为 `.app` bundle 目录。
fn is_app_bundle_path(path: &Path) -> bool {
    path.extension()
        .map(|extension| extension.to_string_lossy() == "app")
        .unwrap_or(false)
}

/// 在解压目录中查找唯一的顶层 `.app` bundle。
fn find_extracted_app_bundle(extract_dir: &Path) -> Result<PathBuf, UpgradeError> {
    let mut app_bundles = Vec::new();
    for entry in WalkDir::new(extract_dir) {
        let entry = entry.map_err(|error| UpgradeError::InvalidPackage(error.to_string()))?;
        if entry.file_type().is_dir()
            && is_app_bundle_path(entry.path())
            && !has_app_bundle_ancestor(entry.path(), extract_dir)
        {
            app_bundles.push(entry.path().to_path_buf());
        }
    }

    match app_bundles.len() {
        1 => Ok(app_bundles.remove(0)),
        0 => Err(UpgradeError::InvalidPackage(
            "macOS 升级包中没有找到 .app bundle".to_string(),
        )),
        _ => Err(UpgradeError::InvalidPackage(
            "macOS 升级包中包含多个 .app bundle".to_string(),
        )),
    }
}

/// 判断路径在解压根目录之下是否已经位于另一个 `.app` bundle 内。
fn has_app_bundle_ancestor(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root)
        .ok()
        .map(|relative| {
            relative
                .ancestors()
                .skip(1)
                .any(|ancestor| !ancestor.as_os_str().is_empty() && is_app_bundle_path(ancestor))
        })
        .unwrap_or(false)
}

/// 校验新 `.app` 中存在和当前进程同名的主可执行文件。
fn ensure_app_bundle_executable(
    app_bundle: &Path,
    executable_name: &OsString,
) -> Result<(), UpgradeError> {
    let executable_path = app_bundle
        .join("Contents")
        .join("MacOS")
        .join(executable_name);
    if !executable_path.is_file() {
        return Err(UpgradeError::InvalidPackage(format!(
            "macOS .app 缺少主可执行文件：{}",
            executable_path.display()
        )));
    }
    make_file_executable_if_needed(&executable_path)
}

/// 计算字节内容的 SHA-256 十六进制摘要。
fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// 生成安全的版本目录名，避免远端版本字符串影响本地路径结构。
fn sanitize_version_for_path(version: &str) -> String {
    let sanitized = version
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => ch,
            _ => '_',
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "unknown".to_string()
    } else {
        sanitized
    }
}

/// 删除已存在的目录；不存在时视为成功。
fn remove_dir_if_exists(path: &Path) -> Result<(), UpgradeError> {
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

/// 生成当前 `.app` 同级临时目录，保证后续 rename 在同一目录内完成。
fn app_bundle_sibling_path(current_bundle: &Path, marker: &str) -> Result<PathBuf, UpgradeError> {
    let parent = current_bundle.parent().ok_or_else(|| {
        UpgradeError::Replace(format!("无法定位 {} 的父目录", current_bundle.display()))
    })?;
    let file_name = current_bundle.file_name().ok_or_else(|| {
        UpgradeError::Replace(format!("无法识别 .app 名称：{}", current_bundle.display()))
    })?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| UpgradeError::Replace(error.to_string()))?
        .as_millis();
    let sibling_name = format!(
        ".{}.{marker}-{}-{timestamp}",
        file_name.to_string_lossy(),
        std::process::id()
    );
    Ok(parent.join(sibling_name))
}

/// 递归复制目录，保留普通文件权限并处理 Unix 符号链接。
fn copy_dir_all(source: &Path, destination: &Path) -> Result<(), UpgradeError> {
    fs::create_dir_all(destination)?;
    let source_permissions = fs::metadata(source)?.permissions();
    fs::set_permissions(destination, source_permissions)?;

    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let entry_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_all(&entry_path, &destination_path)?;
        } else if file_type.is_file() {
            fs::copy(&entry_path, &destination_path)?;
            let permissions = fs::metadata(&entry_path)?.permissions();
            fs::set_permissions(&destination_path, permissions)?;
        } else if file_type.is_symlink() {
            copy_symlink(&entry_path, &destination_path)?;
        }
    }

    Ok(())
}

/// 复制符号链接；非 Unix 平台不应进入 macOS `.app` 替换分支。
#[cfg(unix)]
fn copy_symlink(source: &Path, destination: &Path) -> Result<(), UpgradeError> {
    use std::os::unix::fs::symlink;

    let target = fs::read_link(source)?;
    symlink(target, destination)?;
    Ok(())
}

/// 非 Unix 平台遇到符号链接时返回明确错误。
#[cfg(not(unix))]
fn copy_symlink(source: &Path, _destination: &Path) -> Result<(), UpgradeError> {
    Err(UpgradeError::Replace(format!(
        "当前平台不支持复制符号链接：{}",
        source.display()
    )))
}

/// Base64 解码到固定长度数组，避免签名和公钥长度在后续逻辑中被隐式截断。
fn decode_base64_fixed<const N: usize>(value: &str) -> Result<[u8; N], UpgradeError> {
    let bytes = BASE64_STANDARD
        .decode(value.trim())
        .map_err(|error| UpgradeError::InvalidSignature(error.to_string()))?;
    bytes.try_into().map_err(|bytes: Vec<u8>| {
        UpgradeError::InvalidSignature(format!("期望 {N} 字节，实际 {} 字节", bytes.len()))
    })
}

/// Unix 平台给下载后的替换文件补可执行权限；Windows 不需要该步骤。
#[cfg(unix)]
fn make_file_executable_if_needed(path: &Path) -> Result<(), UpgradeError> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

/// 非 Unix 平台保留同名函数，便于下载流程跨平台复用。
#[cfg(not(unix))]
fn make_file_executable_if_needed(_path: &Path) -> Result<(), UpgradeError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use ed25519_dalek::{Signer, SigningKey};
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    use super::*;
    use crate::config::paths::isolated_test_dir;

    /// 内存 HTTP 客户端，用于模拟 manifest、签名和二进制下载。
    #[derive(Clone, Debug, Default)]
    struct FakeHttpClient {
        responses: HashMap<String, Vec<u8>>,
    }

    impl FakeHttpClient {
        /// 注册一个固定响应体。
        fn with_response(mut self, url: &str, bytes: impl Into<Vec<u8>>) -> Self {
            self.responses.insert(url.to_string(), bytes.into());
            self
        }
    }

    impl UpgradeHttpClient for FakeHttpClient {
        /// 从内存映射读取响应体。
        fn get_bytes(&self, url: &str) -> Result<Vec<u8>, UpgradeError> {
            self.responses
                .get(url)
                .cloned()
                .ok_or_else(|| UpgradeError::Network(format!("missing {url}")))
        }
    }

    /// 记录替换和重启调用的假替换器。
    #[derive(Clone, Debug, Default)]
    struct FakeBinaryReplacer {
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl BinaryReplacer for FakeBinaryReplacer {
        /// 记录替换文件路径，不触碰测试进程。
        fn replace_current_binary(&self, replacement_path: &Path) -> Result<(), UpgradeError> {
            self.calls
                .lock()
                .expect("测试记录锁不应中毒")
                .push(format!("replace:{}", replacement_path.display()));
            Ok(())
        }

        /// 记录 `.app` 替换路径，不触碰测试目录外的内容。
        fn replace_current_app_bundle(
            &self,
            current_bundle: &Path,
            replacement_bundle: &Path,
        ) -> Result<(), UpgradeError> {
            self.calls.lock().expect("测试记录锁不应中毒").push(format!(
                "replace_app:{}=>{}",
                current_bundle.display(),
                replacement_bundle.display()
            ));
            Ok(())
        }

        /// 记录重启路径，不启动新进程。
        fn restart_current_binary(&self, current_exe: &Path) -> Result<(), UpgradeError> {
            self.calls
                .lock()
                .expect("测试记录锁不应中毒")
                .push(format!("restart:{}", current_exe.display()));
            Ok(())
        }

        /// 记录 `.app` 重启路径，不调用系统 `open`。
        fn restart_app_bundle(&self, app_bundle: &Path) -> Result<(), UpgradeError> {
            self.calls
                .lock()
                .expect("测试记录锁不应中毒")
                .push(format!("restart_app:{}", app_bundle.display()));
            Ok(())
        }
    }

    /// 构造测试升级服务和已签名 manifest。
    fn signed_service(
        manifest_bytes: Vec<u8>,
        binary_bytes: Vec<u8>,
    ) -> UpgradeService<FakeHttpClient, Ed25519ManifestVerifier, FakeBinaryReplacer> {
        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let public_key = BASE64_STANDARD.encode(signing_key.verifying_key().to_bytes());
        let signature = BASE64_STANDARD.encode(signing_key.sign(&manifest_bytes).to_bytes());
        let http = FakeHttpClient::default()
            .with_response(
                "https://updates.example.com/manifest-v1.json",
                manifest_bytes,
            )
            .with_response(
                "https://updates.example.com/manifest-v1.json.sig",
                signature.into_bytes(),
            )
            .with_response("https://updates.example.com/argus.bin", binary_bytes);

        UpgradeService::new(
            http,
            Ed25519ManifestVerifier::from_public_key(public_key),
            FakeBinaryReplacer::default(),
            isolated_test_dir("updater-service"),
        )
    }

    /// 生成当前平台可用的 manifest JSON。
    fn manifest_json(version: &str, binary_bytes: &[u8]) -> Vec<u8> {
        format!(
            r#"{{
                "version": "{version}",
                "release_notes": "修复日志读取问题",
                "published_at": "2026-06-15T12:00:00Z",
                "assets": [{{
                    "os": "{}",
                    "arch": "{}",
                    "url": "argus.bin",
                    "sha256": "{}",
                    "size_bytes": {}
                }}]
            }}"#,
            current_platform_os(),
            current_platform_arch(),
            sha256_hex(binary_bytes),
            binary_bytes.len()
        )
        .into_bytes()
    }

    /// 构造包含 `Argus.app` 的测试 zip 包。
    fn app_bundle_zip_bytes(executable_bytes: &[u8]) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut zip = ZipWriter::new(cursor);
        let dir_options = SimpleFileOptions::default().unix_permissions(0o755);
        let file_options = SimpleFileOptions::default().unix_permissions(0o755);

        zip.add_directory("Argus.app/", dir_options)
            .expect("测试 zip 应可写入 app 根目录");
        zip.add_directory("Argus.app/Contents/", dir_options)
            .expect("测试 zip 应可写入 Contents");
        zip.add_directory("Argus.app/Contents/MacOS/", dir_options)
            .expect("测试 zip 应可写入 MacOS");
        zip.start_file("Argus.app/Contents/MacOS/argus", file_options)
            .expect("测试 zip 应可写入主程序");
        zip.write_all(executable_bytes)
            .expect("测试 zip 应可写入主程序内容");
        zip.finish().expect("测试 zip 应可完成").into_inner()
    }

    /// 验证安装目标会在 macOS 裸二进制环境下拒绝自动升级。
    #[test]
    fn install_target_rejects_macos_bare_binary() {
        if cfg!(target_os = "macos") {
            let error = install_target_for_exe_path(Path::new("/tmp/argus"))
                .expect_err("macOS 裸二进制不应进入 self-replace 升级分支");

            assert!(matches!(error, UpgradeError::UnsupportedInstallTarget(_)));
        } else {
            let target = install_target_for_exe_path(Path::new("/tmp/argus"))
                .expect("非 macOS 裸二进制仍使用 self-replace");

            assert!(matches!(target, InstallTarget::CurrentBinary));
        }
    }

    /// 验证高版本 manifest 会返回可安装升级。
    #[test]
    fn check_for_update_returns_available_update() {
        let binary = b"new-binary".to_vec();
        let service = signed_service(manifest_json("0.2.0", &binary), binary);
        let config = UpgradeConfig {
            enabled: true,
            server_url: "https://updates.example.com".to_string(),
            public_key_base64: "configured-by-service".to_string(),
            skipped_version: None,
            last_check_at: None,
        };

        let outcome = service
            .check_for_update(&config, "0.1.0", true)
            .expect("签名有效的新版本应可检查成功");

        match outcome {
            UpgradeCheckOutcome::Available(upgrade) => {
                assert_eq!(upgrade.version, "0.2.0");
                assert_eq!(upgrade.asset.url, "https://updates.example.com/argus.bin");
            }
            other => panic!("期望发现升级，实际为 {other:?}"),
        }
    }

    /// 验证用户跳过的版本不会在自动检查时再次弹出。
    #[test]
    fn check_for_update_respects_skipped_version() {
        let binary = b"new-binary".to_vec();
        let service = signed_service(manifest_json("0.2.0", &binary), binary);
        let config = UpgradeConfig {
            enabled: true,
            server_url: "https://updates.example.com".to_string(),
            public_key_base64: "configured-by-service".to_string(),
            skipped_version: Some("0.2.0".to_string()),
            last_check_at: None,
        };

        let outcome = service
            .check_for_update(&config, "0.1.0", true)
            .expect("跳过版本也应完成检查");

        assert_eq!(outcome, UpgradeCheckOutcome::Skipped("0.2.0".to_string()));
    }

    /// 验证缺少验签公钥时不会继续拉取或解析升级清单。
    #[test]
    fn check_for_update_requires_public_key() {
        let service = UpgradeService::new(
            FakeHttpClient::default(),
            Ed25519ManifestVerifier::from_public_key(String::new()),
            FakeBinaryReplacer::default(),
            isolated_test_dir("updater-missing-key"),
        );
        let config = UpgradeConfig {
            enabled: true,
            server_url: "https://updates.example.com".to_string(),
            public_key_base64: String::new(),
            skipped_version: None,
            last_check_at: None,
        };

        let error = service
            .check_for_update(&config, "0.1.0", true)
            .expect_err("缺少验签公钥必须拒绝升级检查");

        assert!(matches!(error, UpgradeError::MissingPublicKey));
    }

    /// 验证签名错误会阻止升级清单进入解析和安装流程。
    #[test]
    fn invalid_signature_is_rejected() {
        let binary = b"new-binary".to_vec();
        let manifest = manifest_json("0.2.0", &binary);
        let service = UpgradeService::new(
            FakeHttpClient::default()
                .with_response("https://updates.example.com/manifest-v1.json", manifest)
                .with_response("https://updates.example.com/manifest-v1.json.sig", b"bad"),
            Ed25519ManifestVerifier::from_public_key(BASE64_STANDARD.encode([8_u8; 32])),
            FakeBinaryReplacer::default(),
            isolated_test_dir("updater-invalid-signature"),
        );
        let config = UpgradeConfig {
            enabled: true,
            server_url: "https://updates.example.com".to_string(),
            public_key_base64: "configured-by-service".to_string(),
            skipped_version: None,
            last_check_at: None,
        };

        let error = service
            .check_for_update(&config, "0.1.0", true)
            .expect_err("无效签名必须拒绝");

        assert!(matches!(
            error,
            UpgradeError::InvalidSignature(_) | UpgradeError::SignatureVerification(_)
        ));
    }

    /// 验证平台资源选择要求 OS 和架构都匹配。
    #[test]
    fn select_platform_asset_requires_exact_match() {
        let manifest = UpgradeManifest {
            version: "0.2.0".to_string(),
            release_notes: String::new(),
            published_at: String::new(),
            assets: vec![UpgradeAsset {
                os: "windows".to_string(),
                arch: "x86_64".to_string(),
                url: "https://example.com/argus.exe".to_string(),
                sha256: "abc".to_string(),
                size_bytes: 1,
            }],
        };

        assert!(select_platform_asset(&manifest, "macos", "x86_64").is_none());
        assert!(select_platform_asset(&manifest, "windows", "x86_64").is_some());
    }

    /// 验证下载后的二进制必须通过 SHA-256 校验。
    #[test]
    fn download_rejects_hash_mismatch() {
        let binary = b"new-binary".to_vec();
        let service = signed_service(manifest_json("0.2.0", &binary), b"tampered".to_vec());
        let upgrade = AvailableUpgrade {
            version: "0.2.0".to_string(),
            release_notes: String::new(),
            published_at: String::new(),
            asset: UpgradeAsset {
                os: current_platform_os().to_string(),
                arch: current_platform_arch().to_string(),
                url: "https://updates.example.com/argus.bin".to_string(),
                sha256: sha256_hex(&binary),
                size_bytes: b"tampered".len() as u64,
            },
        };

        let error = service
            .download_and_prepare_for_target(&upgrade, InstallTarget::CurrentBinary)
            .expect_err("哈希不一致时必须拒绝安装");

        assert!(matches!(error, UpgradeError::HashMismatch));
    }

    /// 验证 macOS `.app` zip 包会解压为可替换的 app bundle。
    #[test]
    fn download_prepares_macos_app_bundle_package() {
        let app_zip = app_bundle_zip_bytes(b"new-macos-binary");
        let updates_dir = isolated_test_dir("updater-app-bundle");
        let service = UpgradeService::new(
            FakeHttpClient::default()
                .with_response("https://updates.example.com/Argus.app.zip", app_zip.clone()),
            Ed25519ManifestVerifier::from_public_key(BASE64_STANDARD.encode([8_u8; 32])),
            FakeBinaryReplacer::default(),
            updates_dir,
        );
        let upgrade = AvailableUpgrade {
            version: "0.2.0".to_string(),
            release_notes: String::new(),
            published_at: String::new(),
            asset: UpgradeAsset {
                os: "macos".to_string(),
                arch: "aarch64".to_string(),
                url: "https://updates.example.com/Argus.app.zip".to_string(),
                sha256: sha256_hex(&app_zip),
                size_bytes: app_zip.len() as u64,
            },
        };

        let prepared = service
            .download_and_prepare_for_target(
                &upgrade,
                InstallTarget::MacAppBundle {
                    current_bundle: PathBuf::from("/Applications/Argus.app"),
                    executable_name: OsString::from("argus"),
                },
            )
            .expect("有效 app zip 应可准备为 .app bundle");

        assert!(prepared.replacement_path.ends_with("Argus.app"));
        assert!(
            prepared
                .replacement_path
                .join("Contents/MacOS/argus")
                .is_file()
        );
        assert!(matches!(
            prepared.install_target,
            PreparedInstallTarget::MacAppBundle { .. }
        ));
    }

    /// 验证 app bundle 安装路径会替换整个 `.app` 并重启 bundle。
    #[test]
    fn install_prepared_upgrade_uses_app_bundle_replacer() {
        let replacer = FakeBinaryReplacer::default();
        let calls = replacer.calls.clone();
        let service = UpgradeService::new(
            FakeHttpClient::default(),
            Ed25519ManifestVerifier::from_public_key(BASE64_STANDARD.encode([8_u8; 32])),
            replacer,
            isolated_test_dir("updater-install-app-bundle"),
        );
        let prepared = PreparedUpgrade {
            version: "0.2.0".to_string(),
            replacement_path: PathBuf::from("/tmp/Argus.app"),
            install_target: PreparedInstallTarget::MacAppBundle {
                current_bundle: PathBuf::from("/Applications/Argus.app"),
            },
        };

        service
            .install_prepared_upgrade(&prepared)
            .expect("app bundle 安装应调用替换器");

        let calls = calls.lock().expect("测试记录锁不应中毒");
        assert_eq!(
            calls.as_slice(),
            [
                "replace_app:/Applications/Argus.app=>/tmp/Argus.app",
                "restart_app:/Applications/Argus.app"
            ]
        );
    }

    /// 验证 SemVer 比较兼容 v 前缀。
    #[test]
    fn remote_version_comparison_accepts_v_prefix() {
        assert!(is_remote_version_newer("0.1.0", "v0.2.0").expect("版本号应可解析"));
        assert!(!is_remote_version_newer("0.2.0", "v0.2.0").expect("版本号应可解析"));
    }

    /// 验证版本字符串不会直接影响本地目录结构。
    #[test]
    fn version_path_is_sanitized() {
        assert_eq!(
            sanitize_version_for_path("v1.2.3/../../x"),
            "v1.2.3_.._.._x"
        );
    }
}
