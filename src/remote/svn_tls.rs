//! 文件职责：对 SVN HTTPS 执行严格证书校验，并提供按地址和证书指纹限定的显式信任例外。
//! 创建日期：2026-10-29
//! 作者：Argus 开发团队

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, Error, RootCertStore, SignatureScheme,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::{Host, Url};
use x509_parser::extensions::GeneralName;
use x509_parser::prelude::{FromDer, X509Certificate};

/// 限制不受信任服务端提供的证书数和候选签名验证次数，避免环状或分叉链耗尽资源。
const SVN_MAX_CHAIN_CERTIFICATES: usize = 64;
const SVN_MAX_CHAIN_SIGNATURE_CHECKS: usize = 128;

/// 可由用户针对某一张证书确认的异常；有效期、格式、用途和签名错误始终拒绝。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SvnCertificateFailure {
    UnknownIssuer,
    NameMismatch,
}

impl SvnCertificateFailure {
    /// 返回证书确认界面中可直接展示的中文原因。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::UnknownIssuer => "无法建立受信任的证书链（签发机构未知或中间证书缺失）",
            Self::NameMismatch => "访问地址与证书允许的域名或 IP 不匹配",
        }
    }
}

/// 用户确认的单个 HTTPS 地址和证书指纹；所属连接由配置层另外限定。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct SvnCertificateTrust {
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
    pub allowed_failures: Vec<SvnCertificateFailure>,
}

impl SvnCertificateTrust {
    /// 检查持久化记录的格式；指纹使用固定的 SHA-256 小写十六进制表示。
    pub(crate) fn is_valid(&self) -> bool {
        let Some(digest) = self.fingerprint.strip_prefix("SHA256:") else {
            return false;
        };
        if self.port == 0
            || digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || self
                .allowed_failures
                .iter()
                .enumerate()
                .any(|(index, failure)| self.allowed_failures[..index].contains(failure))
        {
            return false;
        }
        // 存储地址不带 IPv6 方括号，不能直接按 URL 中的 host 语法解析。
        if let Ok(address) = self.host.parse::<IpAddr>() {
            return address.to_string() == self.host;
        }
        let Ok(host) = Host::parse(&self.host) else {
            return false;
        };
        let canonical = match host {
            Host::Domain(domain) => domain,
            Host::Ipv4(address) => address.to_string(),
            Host::Ipv6(address) => address.to_string(),
        };
        !canonical.is_empty() && canonical == self.host
    }

    /// 只允许当前 HTTPS 主机和端口复用确认结果，不扩大到其他连接地址或协议。
    pub(crate) fn matches_url(&self, url: &Url) -> bool {
        self.is_valid()
            && svn_https_endpoint(url)
                .is_some_and(|(host, port)| host == self.host && port == self.port)
    }
}

/// 一次失败握手提取出的公开证书信息；不保存原始证书、私钥或连接凭据。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SvnCertificatePrompt {
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
    pub subject: String,
    pub issuer: String,
    pub subject_alt_names: Vec<String>,
    pub not_before: String,
    pub not_after: String,
    pub failures: Vec<SvnCertificateFailure>,
    pub previous_fingerprint: Option<String>,
}

/// 每个远程会话独立保存审批记录；TLS 回调只记录信息，不等待用户或持有网络连接。
#[derive(Debug, Default)]
struct SvnTlsTrustState {
    trusted: Option<SvnCertificateTrust>,
    pending: Option<SvnCertificatePrompt>,
    latest_challenge: Option<SvnCertificatePrompt>,
}

/// 可交给 ureq 的 TLS 配置及当前握手产生的证书确认请求。
pub(crate) struct SvnTlsSession {
    config: Arc<ClientConfig>,
    verifier: Arc<SvnServerCertVerifier>,
}

impl SvnTlsSession {
    /// 为一个 HTTPS 会话加载当前系统根证书；加载失败显式报错，不以空根库掩盖失败。
    pub(crate) fn new(url: &Url, trusted: Option<SvnCertificateTrust>) -> Result<Self> {
        let loaded = rustls_native_certs::load_native_certs();
        if !loaded.errors.is_empty() {
            bail!("加载 SVN HTTPS 系统证书库失败：{:?}", loaded.errors);
        }
        let mut roots = RootCertStore::empty();
        for certificate in loaded.certs {
            roots
                .add(certificate)
                .context("解析 SVN HTTPS 系统根证书失败")?;
        }
        if roots.is_empty() {
            bail!("SVN HTTPS 系统证书库为空，请检查操作系统信任设置");
        }
        Self::with_roots(url, trusted, roots)
    }

    /// 通过指定根证书构造会话，正常路径和隔离测试共用完整校验链。
    fn with_roots(
        url: &Url,
        trusted: Option<SvnCertificateTrust>,
        roots: RootCertStore,
    ) -> Result<Self> {
        let (host, port) =
            svn_https_endpoint(url).ok_or_else(|| anyhow!("SVN 证书校验需要有效的 HTTPS 地址"))?;
        if trusted
            .as_ref()
            .is_some_and(|trust| !trust.matches_url(url))
        {
            bail!("保存的 SVN 证书信任记录与当前 HTTPS 地址不一致");
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let strict = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
            .build()
            .context("创建 SVN HTTPS 证书验证器失败")?;
        let verifier = Arc::new(SvnServerCertVerifier {
            host,
            port,
            strict,
            state: Mutex::new(SvnTlsTrustState {
                trusted,
                ..Default::default()
            }),
        });
        let mut config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .context("配置 SVN HTTPS 协议失败")?
            .dangerous()
            .with_custom_certificate_verifier(verifier.clone())
            .with_no_client_auth();
        // 恢复 TLS 会话时不再发送证书，因此禁用恢复以保证每次新连接都检查证书变化。
        config.resumption = rustls::client::Resumption::disabled();
        Ok(Self {
            config: Arc::new(config),
            verifier,
        })
    }

    /// 返回当前会话专属配置；不与其他服务器或连接共享信任例外。
    pub(crate) fn client_config(&self) -> Arc<ClientConfig> {
        self.config.clone()
    }

    /// 提取最近失败握手的审批信息；保留挑战快照，以拒绝过期或被篡改的审批响应。
    pub(crate) fn take_pending(&self) -> Result<Option<SvnCertificatePrompt>> {
        let mut state = self
            .verifier
            .state
            .lock()
            .map_err(|_| anyhow!("SVN 证书审批状态已损坏"))?;
        Ok(state.pending.take())
    }

    /// 应用用户对最近一次证书挑战的确认；返回仅包含公开指纹的可持久化记录。
    pub(crate) fn trust(&self, prompt: &SvnCertificatePrompt) -> Result<SvnCertificateTrust> {
        let mut state = self
            .verifier
            .state
            .lock()
            .map_err(|_| anyhow!("SVN 证书审批状态已损坏"))?;
        if state.latest_challenge.as_ref() != Some(prompt) {
            bail!("SVN 证书确认已过期，请重新连接并核对最新证书");
        }
        let trusted = SvnCertificateTrust {
            host: prompt.host.clone(),
            port: prompt.port,
            fingerprint: prompt.fingerprint.clone(),
            allowed_failures: prompt.failures.clone(),
        };
        state.trusted = Some(trusted.clone());
        state.pending = None;
        state.latest_challenge = None;
        Ok(trusted)
    }
}

/// 将 URL 转为不带凭据、路径和 IPv6 方括号的 HTTPS 地址标识。
pub(crate) fn svn_https_endpoint(url: &Url) -> Option<(String, u16)> {
    if url.scheme() != "https" {
        return None;
    }
    let host = match url.host()? {
        Host::Domain(domain) => domain.to_string(),
        Host::Ipv4(address) => address.to_string(),
        Host::Ipv6(address) => address.to_string(),
    };
    let port = url.port_or_known_default()?;
    (!host.is_empty() && port > 0).then_some((host, port))
}

/// 严格验证失败后仅允许用户批准过的异常；服务器仍须证明持有该证书对应的私钥。
#[derive(Debug)]
struct SvnServerCertVerifier {
    host: String,
    port: u16,
    strict: Arc<WebPkiServerVerifier>,
    state: Mutex<SvnTlsTrustState>,
}

impl ServerCertVerifier for SvnServerCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, Error> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::General("SVN 证书审批状态已损坏".into()))?;
        state.pending = None;
        state.latest_challenge = None;
        if server_name.to_str().as_ref() != self.host {
            return Err(Error::General("SVN HTTPS 证书验证地址发生变化".into()));
        }
        let certificate = parse_certificate(end_entity, now)?;
        let result = self.strict.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        );
        let mut failures = Vec::new();
        match &result {
            Ok(_) => {}
            Err(Error::InvalidCertificate(CertificateError::UnknownIssuer)) => {
                // 未找到受信根时 WebPKI 尚未验链签名，不能把已提供链的坏签名当成单纯的未知 CA。
                verify_available_chain_signatures(&certificate, intermediates, now)?;
                failures.push(SvnCertificateFailure::UnknownIssuer);
            }
            Err(Error::InvalidCertificate(
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
            )) => failures.push(SvnCertificateFailure::NameMismatch),
            Err(error) => return Err(error.clone()),
        }
        // 链校验先失败时 Rustls 尚未检查名称，必须独立补查，避免用户只看到一半问题。
        let parsed = rustls::server::ParsedCertificate::try_from(end_entity)?;
        match rustls::client::verify_server_name(&parsed, server_name) {
            Ok(()) => {}
            Err(Error::InvalidCertificate(
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
            )) => {
                if !failures.contains(&SvnCertificateFailure::NameMismatch) {
                    failures.push(SvnCertificateFailure::NameMismatch);
                }
            }
            Err(error) => return Err(error),
        }
        let fingerprint = format!(
            "SHA256:{}",
            hex::encode(Sha256::digest(end_entity.as_ref()))
        );
        let previous_fingerprint = state
            .trusted
            .as_ref()
            .map(|trust| trust.fingerprint.clone());
        let changed = previous_fingerprint
            .as_ref()
            .is_some_and(|previous| previous != &fingerprint);
        let approved = state.trusted.as_ref().is_some_and(|trust| {
            trust.host == self.host
                && trust.port == self.port
                && trust.fingerprint == fingerprint
                && failures
                    .iter()
                    .all(|failure| trust.allowed_failures.contains(failure))
        });
        if approved || (result.is_ok() && !changed) {
            return Ok(ServerCertVerified::assertion());
        }
        let prompt = certificate_prompt(
            &certificate,
            &self.host,
            self.port,
            fingerprint,
            failures,
            previous_fingerprint,
        )?;
        state.pending = Some(prompt.clone());
        state.latest_challenge = Some(prompt);
        Err(result
            .err()
            .unwrap_or_else(|| Error::General("SVN HTTPS 服务器证书已更换，需要重新确认".into())))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, Error> {
        self.strict.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, Error> {
        self.strict.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.strict.supported_verify_schemes()
    }
}

/// 展示信息和有效期检查共用成熟的 X.509 解析器；异常编码或时间永远不能进入信任例外。
fn parse_certificate<'a>(
    certificate: &'a CertificateDer<'_>,
    now: UnixTime,
) -> std::result::Result<X509Certificate<'a>, Error> {
    let parsed = parse_certificate_der(certificate)?;
    verify_certificate_validity(&parsed, now)?;
    Ok(parsed)
}

/// 先读取链结构再决定是否使用某个签发者，无关额外证书的日期不能阻断叶证书确认。
fn parse_certificate_der<'a>(
    certificate: &'a CertificateDer<'_>,
) -> std::result::Result<X509Certificate<'a>, Error> {
    let (remaining, parsed) = X509Certificate::from_der(certificate.as_ref())
        .map_err(|_| Error::InvalidCertificate(CertificateError::BadEncoding))?;
    if !remaining.is_empty() {
        return Err(Error::InvalidCertificate(CertificateError::BadEncoding));
    }
    Ok(parsed)
}

/// 仅在证书确实用于当前叶证书的候选链时检查日期；叶证书始终检查。
fn verify_certificate_validity(
    certificate: &X509Certificate<'_>,
    now: UnixTime,
) -> std::result::Result<(), Error> {
    let now = i64::try_from(now.as_secs())
        .map_err(|_| Error::InvalidCertificate(CertificateError::BadEncoding))?;
    if now < certificate.validity().not_before.timestamp() {
        return Err(Error::InvalidCertificate(CertificateError::NotValidYet));
    }
    if now > certificate.validity().not_after.timestamp() {
        return Err(Error::InvalidCertificate(CertificateError::Expired));
    }
    Ok(())
}

/// 仅从叶证书沿可连接的签发者验签；缺失签发者保留为待用户确认的 UnknownIssuer。
fn verify_available_chain_signatures(
    leaf: &X509Certificate<'_>,
    intermediates: &[CertificateDer<'_>],
    now: UnixTime,
) -> std::result::Result<(), Error> {
    if intermediates.len() >= SVN_MAX_CHAIN_CERTIFICATES {
        return Err(Error::General("SVN HTTPS 证书链过长".into()));
    }
    let parsed_intermediates = intermediates
        .iter()
        .map(parse_certificate_der)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let certificates: Vec<_> = std::iter::once(leaf)
        .chain(parsed_intermediates.iter())
        .collect();
    let mut remaining_signature_checks = SVN_MAX_CHAIN_SIGNATURE_CHECKS;
    verify_certificate_path(
        0,
        &certificates,
        now,
        &mut vec![0],
        &mut remaining_signature_checks,
    )
}

/// 有多个同名签发者时允许替代链；任何一条可验证路径成功即可，全部失败则保留明确错误。
fn verify_certificate_path(
    certificate_index: usize,
    certificates: &[&X509Certificate<'_>],
    now: UnixTime,
    path: &mut Vec<usize>,
    remaining_signature_checks: &mut usize,
) -> std::result::Result<(), Error> {
    let certificate = certificates[certificate_index];
    verify_certificate_validity(certificate, now)?;
    let mut has_issuer = false;
    let mut failure = Error::InvalidCertificate(CertificateError::BadSignature);
    for (issuer_index, issuer) in certificates.iter().enumerate() {
        if issuer.subject() != certificate.issuer() {
            continue;
        }
        has_issuer = true;
        *remaining_signature_checks = remaining_signature_checks
            .checked_sub(1)
            .ok_or_else(|| Error::General("SVN HTTPS 证书链候选过多".into()))?;
        if certificate
            .verify_signature(Some(issuer.public_key()))
            .is_err()
        {
            failure = Error::InvalidCertificate(CertificateError::BadSignature);
            continue;
        }
        if issuer_index == certificate_index {
            return Ok(());
        }
        if path.contains(&issuer_index) {
            failure = Error::General("SVN HTTPS 证书链存在循环".into());
            continue;
        }
        let previous_path_length = path.len();
        path.push(issuer_index);
        let result = verify_certificate_path(
            issuer_index,
            certificates,
            now,
            path,
            remaining_signature_checks,
        );
        path.truncate(previous_path_length);
        match result {
            Ok(()) => return Ok(()),
            Err(error) => failure = error,
        }
    }
    if has_issuer { Err(failure) } else { Ok(()) }
}

/// 只生成公开证书摘要，保留全部名称异常供用户核对，不把私钥或认证请求带入确认界面。
fn certificate_prompt(
    certificate: &X509Certificate<'_>,
    host: &str,
    port: u16,
    fingerprint: String,
    failures: Vec<SvnCertificateFailure>,
    previous_fingerprint: Option<String>,
) -> std::result::Result<SvnCertificatePrompt, Error> {
    let subject_alt_names = certificate
        .subject_alternative_name()
        .map_err(|_| Error::InvalidCertificate(CertificateError::BadEncoding))?
        .map(|extension| {
            extension
                .value
                .general_names
                .iter()
                .filter_map(|name| match name {
                    GeneralName::DNSName(name) => Some((*name).to_string()),
                    GeneralName::IPAddress(bytes) if bytes.len() == 4 => Some(
                        IpAddr::V4(Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]))
                            .to_string(),
                    ),
                    GeneralName::IPAddress(bytes) if bytes.len() == 16 => {
                        let mut address = [0; 16];
                        address.copy_from_slice(bytes);
                        Some(IpAddr::V6(Ipv6Addr::from(address)).to_string())
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let display_time = |timestamp| {
        DateTime::<Utc>::from_timestamp(timestamp, 0)
            .map(|time| time.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .ok_or(Error::InvalidCertificate(CertificateError::BadEncoding))
    };
    Ok(SvnCertificatePrompt {
        host: host.to_string(),
        port,
        fingerprint,
        subject: certificate.subject().to_string(),
        issuer: certificate.issuer().to_string(),
        subject_alt_names,
        not_before: display_time(certificate.validity().not_before.timestamp())?,
        not_after: display_time(certificate.validity().not_after.timestamp())?,
        failures,
        previous_fingerprint,
    })
}

#[cfg(test)]
mod tests;
