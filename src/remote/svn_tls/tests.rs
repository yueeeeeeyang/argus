//! 文件职责：使用临时生成证书和内存 TLS 握手验证 SVN 证书信任边界，不访问网络。
//! 创建日期：2026-10-29
//! 作者：Argus 开发团队

use std::io::Cursor;
use std::time::Duration;

use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, date_time_ymd,
};
use rustls::pki_types::PrivatePkcs8KeyDer;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::{ClientConnection, ServerConfig, ServerConnection, SupportedProtocolVersion};

use super::*;

struct TestAuthority {
    params: CertificateParams,
    key: KeyPair,
    certificate: Certificate,
}

impl TestAuthority {
    fn new(name: &str) -> Self {
        let mut params = CertificateParams::default();
        params.distinguished_name.push(DnType::CommonName, name);
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let key = KeyPair::generate().unwrap();
        let certificate = params.self_signed(&key).unwrap();
        Self {
            params,
            key,
            certificate,
        }
    }

    fn roots(&self) -> RootCertStore {
        let mut roots = RootCertStore::empty();
        roots.add(self.certificate.der().clone()).unwrap();
        roots
    }

    fn issue(&self, params: CertificateParams) -> TestCertificate {
        let key = KeyPair::generate().unwrap();
        let certificate = params
            .signed_by(&key, &Issuer::from_params(&self.params, &self.key))
            .unwrap();
        TestCertificate { certificate, key }
    }
}

struct TestCertificate {
    certificate: Certificate,
    key: KeyPair,
}

fn leaf_params(names: &[&str]) -> CertificateParams {
    let mut params = CertificateParams::new(
        names
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    params
        .distinguished_name
        .push(DnType::CommonName, "SVN 测试服务器");
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params
}

fn session(roots: RootCertStore, trusted: Option<SvnCertificateTrust>) -> SvnTlsSession {
    SvnTlsSession::with_roots(
        &Url::parse("https://repo.test/svn").unwrap(),
        trusted,
        roots,
    )
    .unwrap()
}

fn verify(
    session: &SvnTlsSession,
    certificate: &CertificateDer<'_>,
    intermediates: &[CertificateDer<'_>],
) -> std::result::Result<ServerCertVerified, Error> {
    session.verifier.verify_server_cert(
        certificate,
        intermediates,
        &ServerName::try_from("repo.test").unwrap(),
        &[],
        UnixTime::now(),
    )
}

fn server_config(certificate: &TestCertificate) -> Arc<ServerConfig> {
    Arc::new(
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![certificate.certificate.der().clone()],
                PrivatePkcs8KeyDer::from(certificate.key.serialize_der()).into(),
            )
            .unwrap(),
    )
}

/// 直接交换握手记录，覆盖证书验证和 CertificateVerify，不依赖端口、网络或系统证书库。
fn handshake(session: &SvnTlsSession, server: Arc<ServerConfig>) -> std::result::Result<(), Error> {
    let mut client = ClientConnection::new(
        session.client_config(),
        ServerName::try_from("repo.test").unwrap().to_owned(),
    )
    .unwrap();
    let mut server = ServerConnection::new(server).unwrap();
    for _ in 0..16 {
        let mut outgoing = Vec::new();
        while client.wants_write() {
            client.write_tls(&mut outgoing).unwrap();
        }
        if !outgoing.is_empty() {
            server.read_tls(&mut Cursor::new(outgoing)).unwrap();
            server.process_new_packets()?;
        }
        let mut outgoing = Vec::new();
        while server.wants_write() {
            server.write_tls(&mut outgoing).unwrap();
        }
        if !outgoing.is_empty() {
            client.read_tls(&mut Cursor::new(outgoing)).unwrap();
            client.process_new_packets()?;
        }
        if !client.is_handshaking() && !server.is_handshaking() {
            return Ok(());
        }
    }
    panic!("内存 TLS 握手未在限定轮数内结束");
}

#[test]
fn valid_certificate_connects_without_prompt() {
    let authority = TestAuthority::new("受信测试 CA");
    let certificate = authority.issue(leaf_params(&["repo.test"]));
    let session = session(authority.roots(), None);
    handshake(&session, server_config(&certificate)).unwrap();
    assert!(session.take_pending().unwrap().is_none());
}

#[test]
fn unknown_issuer_and_name_mismatch_require_explicit_approval() {
    let trusted_authority = TestAuthority::new("受信测试 CA");
    let unknown_authority = TestAuthority::new("内部测试 CA");
    let certificate =
        unknown_authority.issue(leaf_params(&["another.test", "192.0.2.10", "2001:db8::1"]));
    let session = session(trusted_authority.roots(), None);
    assert!(handshake(&session, server_config(&certificate)).is_err());
    let prompt = session.take_pending().unwrap().unwrap();
    assert_eq!(
        prompt.failures,
        vec![
            SvnCertificateFailure::UnknownIssuer,
            SvnCertificateFailure::NameMismatch
        ]
    );
    assert_eq!(prompt.host, "repo.test");
    assert_eq!(prompt.port, 443);
    assert_eq!(
        prompt.fingerprint,
        format!(
            "SHA256:{}",
            hex::encode(Sha256::digest(certificate.certificate.der().as_ref()))
        )
    );
    assert!(prompt.subject.contains("SVN 测试服务器"));
    assert!(prompt.issuer.contains("内部测试 CA"));
    assert_eq!(
        prompt.subject_alt_names,
        ["another.test", "192.0.2.10", "2001:db8::1"]
    );
    assert!(prompt.not_before.ends_with(" UTC"));
    assert!(prompt.not_after.ends_with(" UTC"));
    assert!(session.take_pending().unwrap().is_none());
    let trusted = session.trust(&prompt).unwrap();
    assert!(trusted.is_valid());
    handshake(&session, server_config(&certificate)).unwrap();
    assert!(session.take_pending().unwrap().is_none());
    assert!(session.trust(&prompt).is_err());
}

#[test]
fn name_mismatch_from_a_trusted_ca_is_reported_separately() {
    let authority = TestAuthority::new("受信测试 CA");
    let certificate = authority.issue(leaf_params(&["another.test"]));
    let session = session(authority.roots(), None);
    assert!(verify(&session, certificate.certificate.der(), &[]).is_err());
    let prompt = session.take_pending().unwrap().unwrap();
    assert_eq!(prompt.failures, [SvnCertificateFailure::NameMismatch]);
    session.trust(&prompt).unwrap();
    handshake(&session, server_config(&certificate)).unwrap();
}

#[test]
fn approved_self_signed_certificate_can_be_restored_for_the_same_endpoint() {
    let authority = TestAuthority::new("受信测试 CA");
    let key = KeyPair::generate().unwrap();
    let certificate = TestCertificate {
        certificate: leaf_params(&["repo.test"]).self_signed(&key).unwrap(),
        key,
    };
    let original = session(authority.roots(), None);
    assert!(handshake(&original, server_config(&certificate)).is_err());
    let prompt = original.take_pending().unwrap().unwrap();
    assert_eq!(prompt.failures, [SvnCertificateFailure::UnknownIssuer]);
    let trusted = original.trust(&prompt).unwrap();
    let restored = session(authority.roots(), Some(trusted));
    handshake(&restored, server_config(&certificate)).unwrap();
    assert!(restored.take_pending().unwrap().is_none());
}

#[test]
fn valid_unknown_chain_is_eligible_for_explicit_trust() {
    let authority = TestAuthority::new("受信测试 CA");
    let unknown = TestAuthority::new("内部测试 CA");
    let certificate = unknown.issue(leaf_params(&["repo.test"]));
    let session = session(authority.roots(), None);
    let intermediates = [unknown.certificate.der().clone()];
    assert!(verify(&session, certificate.certificate.der(), &intermediates).is_err());
    let prompt = session.take_pending().unwrap().unwrap();
    assert_eq!(prompt.failures, [SvnCertificateFailure::UnknownIssuer]);
    session.trust(&prompt).unwrap();
    verify(&session, certificate.certificate.der(), &intermediates).unwrap();
}

#[test]
fn declining_or_altering_a_prompt_never_grants_trust() {
    let authority = TestAuthority::new("受信测试 CA");
    let unknown = TestAuthority::new("内部测试 CA");
    let certificate = unknown.issue(leaf_params(&["repo.test"]));
    let session = session(authority.roots(), None);
    assert!(verify(&session, certificate.certificate.der(), &[]).is_err());
    let mut prompt = session.take_pending().unwrap().unwrap();
    prompt.host = "different.test".to_string();
    assert!(session.trust(&prompt).is_err());
    assert!(handshake(&session, server_config(&certificate)).is_err());
    assert!(session.verifier.state.lock().unwrap().trusted.is_none());
}

#[test]
fn changed_certificate_even_when_ca_valid_requires_fresh_confirmation() {
    let authority = TestAuthority::new("受信测试 CA");
    let old = authority.issue(leaf_params(&["another.test"]));
    let new = authority.issue(leaf_params(&["repo.test"]));
    let session = session(authority.roots(), None);
    assert!(verify(&session, old.certificate.der(), &[]).is_err());
    let old_prompt = session.take_pending().unwrap().unwrap();
    let old_trust = session.trust(&old_prompt).unwrap();
    assert!(handshake(&session, server_config(&new)).is_err());
    let changed = session.take_pending().unwrap().unwrap();
    assert!(changed.failures.is_empty());
    assert_eq!(
        changed.previous_fingerprint.as_deref(),
        Some(old_trust.fingerprint.as_str())
    );
    assert_ne!(changed.fingerprint, old_trust.fingerprint);
    assert!(session.trust(&old_prompt).is_err());
    session.trust(&changed).unwrap();
    handshake(&session, server_config(&new)).unwrap();
}

#[test]
fn a_new_handshake_invalidates_the_previous_certificate_challenge() {
    let authority = TestAuthority::new("受信测试 CA");
    let unknown = TestAuthority::new("内部测试 CA");
    let first = unknown.issue(leaf_params(&["repo.test"]));
    let second = unknown.issue(leaf_params(&["repo.test"]));
    let session = session(authority.roots(), None);
    assert!(verify(&session, first.certificate.der(), &[]).is_err());
    let first_prompt = session.take_pending().unwrap().unwrap();
    assert!(verify(&session, second.certificate.der(), &[]).is_err());
    assert!(session.trust(&first_prompt).is_err());
    let second_prompt = session.take_pending().unwrap().unwrap();
    session.trust(&second_prompt).unwrap();
    assert!(verify(&session, first.certificate.der(), &[]).is_err());
}

#[test]
fn saved_trust_does_not_expand_its_approved_failures() {
    let authority = TestAuthority::new("受信测试 CA");
    let unknown = TestAuthority::new("内部测试 CA");
    let certificate = unknown.issue(leaf_params(&["another.test"]));
    let original = session(authority.roots(), None);
    assert!(verify(&original, certificate.certificate.der(), &[]).is_err());
    let prompt = original.take_pending().unwrap().unwrap();
    let mut trusted = original.trust(&prompt).unwrap();
    trusted.allowed_failures = vec![SvnCertificateFailure::UnknownIssuer];
    let restored = session(authority.roots(), Some(trusted));
    assert!(verify(&restored, certificate.certificate.der(), &[]).is_err());
    assert_eq!(
        restored.take_pending().unwrap().unwrap().failures,
        prompt.failures
    );
}

#[test]
fn saved_trust_is_limited_to_canonical_https_host_and_port() {
    let trust = SvnCertificateTrust {
        host: "repo.test".into(),
        port: 443,
        fingerprint: format!("SHA256:{}", "a".repeat(64)),
        allowed_failures: vec![],
    };
    assert!(trust.matches_url(&Url::parse("https://REPO.TEST:443/svn/other").unwrap()));
    for address in [
        "http://repo.test/svn",
        "https://different.test/svn",
        "https://repo.test:8443/svn",
    ] {
        let url = Url::parse(address).unwrap();
        assert!(!trust.matches_url(&url));
        let authority = TestAuthority::new("受信测试 CA");
        assert!(SvnTlsSession::with_roots(&url, Some(trust.clone()), authority.roots()).is_err());
    }
    let ipv6 = SvnCertificateTrust {
        host: "2001:db8::1".into(),
        ..trust.clone()
    };
    assert!(ipv6.is_valid());
    assert!(ipv6.matches_url(&Url::parse("https://[2001:0db8::1]/svn").unwrap()));
    assert!(
        !SvnCertificateTrust {
            host: "[2001:db8::1]".into(),
            ..trust.clone()
        }
        .is_valid()
    );
    assert!(
        !SvnCertificateTrust {
            host: "REPO.TEST".into(),
            ..trust.clone()
        }
        .is_valid()
    );
    assert!(
        !SvnCertificateTrust {
            port: 0,
            ..trust.clone()
        }
        .is_valid()
    );
    assert!(
        !SvnCertificateTrust {
            fingerprint: "SHA256:abcd".into(),
            ..trust.clone()
        }
        .is_valid()
    );
    assert!(
        !SvnCertificateTrust {
            allowed_failures: vec![
                SvnCertificateFailure::UnknownIssuer,
                SvnCertificateFailure::UnknownIssuer
            ],
            ..trust
        }
        .is_valid()
    );
}

#[test]
fn verifier_rejects_reuse_for_a_different_server_name() {
    let authority = TestAuthority::new("受信测试 CA");
    let certificate = authority.issue(leaf_params(&["repo.test", "other.test"]));
    let session = session(authority.roots(), None);
    assert!(
        session
            .verifier
            .verify_server_cert(
                certificate.certificate.der(),
                &[],
                &ServerName::try_from("other.test").unwrap(),
                &[],
                UnixTime::now()
            )
            .is_err()
    );
    assert!(session.take_pending().unwrap().is_none());
}

#[test]
fn expired_not_yet_valid_and_malformed_certificates_cannot_be_approved() {
    let authority = TestAuthority::new("受信测试 CA");
    let unknown = TestAuthority::new("内部测试 CA");
    let session = session(authority.roots(), None);
    for (before, after, expected) in [
        (2000, 2001, CertificateError::Expired),
        (2090, 2091, CertificateError::NotValidYet),
    ] {
        let mut params = leaf_params(&["repo.test"]);
        params.not_before = date_time_ymd(before, 1, 1);
        params.not_after = date_time_ymd(after, 1, 1);
        let certificate = unknown.issue(params);
        let now = UnixTime::since_unix_epoch(Duration::from_secs(1_800_000_000));
        assert_eq!(
            session
                .verifier
                .verify_server_cert(
                    certificate.certificate.der(),
                    &[],
                    &ServerName::try_from("repo.test").unwrap(),
                    &[],
                    now
                )
                .unwrap_err(),
            Error::InvalidCertificate(expected)
        );
        assert!(session.take_pending().unwrap().is_none());
    }
    assert_eq!(
        verify(&session, &CertificateDer::from(vec![0, 1, 2, 3]), &[]).unwrap_err(),
        Error::InvalidCertificate(CertificateError::BadEncoding)
    );
    assert!(session.take_pending().unwrap().is_none());
}

#[test]
fn trust_never_bypasses_a_later_expiration_or_invalid_server_purpose() {
    let authority = TestAuthority::new("受信测试 CA");
    let unknown = TestAuthority::new("内部测试 CA");
    let mut params = leaf_params(&["repo.test"]);
    params.not_before = date_time_ymd(2000, 1, 1);
    params.not_after = date_time_ymd(2040, 1, 1);
    let certificate = unknown.issue(params);
    let session = session(authority.roots(), None);
    assert!(verify(&session, certificate.certificate.der(), &[]).is_err());
    session
        .trust(&session.take_pending().unwrap().unwrap())
        .unwrap();
    let after_expiration = UnixTime::since_unix_epoch(Duration::from_secs(2_300_000_000));
    assert_eq!(
        session
            .verifier
            .verify_server_cert(
                certificate.certificate.der(),
                &[],
                &ServerName::try_from("repo.test").unwrap(),
                &[],
                after_expiration
            )
            .unwrap_err(),
        Error::InvalidCertificate(CertificateError::Expired)
    );
    assert!(session.take_pending().unwrap().is_none());
    let mut client_params = leaf_params(&["repo.test"]);
    client_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let client_only = unknown.issue(client_params);
    assert!(verify(&session, client_only.certificate.der(), &[]).is_err());
    assert!(session.take_pending().unwrap().is_none());
}

#[derive(Debug)]
struct MismatchedSigningKey(Arc<CertifiedKey>);

impl ResolvesServerCert for MismatchedSigningKey {
    fn resolve(&self, _client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.0.clone())
    }
}

#[test]
fn trusted_fingerprint_still_requires_valid_tls12_and_tls13_signatures() {
    let authority = TestAuthority::new("受信测试 CA");
    let unknown = TestAuthority::new("内部测试 CA");
    let certificate = unknown.issue(leaf_params(&["repo.test"]));
    let session = session(authority.roots(), None);
    assert!(verify(&session, certificate.certificate.der(), &[]).is_err());
    session
        .trust(&session.take_pending().unwrap().unwrap())
        .unwrap();
    let other_key = PrivatePkcs8KeyDer::from(KeyPair::generate().unwrap().serialize_der()).into();
    let signer = rustls::crypto::ring::sign::any_supported_type(&other_key).unwrap();
    let resolver = Arc::new(MismatchedSigningKey(Arc::new(CertifiedKey::new(
        vec![certificate.certificate.der().clone()],
        signer,
    ))));
    for protocol in [&rustls::version::TLS12, &rustls::version::TLS13] {
        let protocols: &[&SupportedProtocolVersion] = &[protocol];
        let server =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_protocol_versions(protocols)
                .unwrap()
                .with_no_client_auth()
                .with_cert_resolver(resolver.clone());
        assert_eq!(
            handshake(&session, Arc::new(server)).unwrap_err(),
            Error::InvalidCertificate(CertificateError::BadSignature)
        );
        assert!(session.take_pending().unwrap().is_none());
    }
}

#[test]
fn unknown_issuer_cannot_hide_invalid_certificate_chain_signatures() {
    let authority = TestAuthority::new("受信测试 CA");
    let unknown = TestAuthority::new("内部测试 CA");
    let certificate = unknown.issue(leaf_params(&["repo.test"]));
    let session = session(authority.roots(), None);
    let mut corrupted = certificate.certificate.der().as_ref().to_vec();
    *corrupted.last_mut().unwrap() ^= 1;
    assert_eq!(
        verify(
            &session,
            &CertificateDer::from(corrupted),
            &[unknown.certificate.der().clone()]
        )
        .unwrap_err(),
        Error::InvalidCertificate(CertificateError::BadSignature)
    );
    assert!(session.take_pending().unwrap().is_none());
    let key = KeyPair::generate().unwrap();
    let self_signed = leaf_params(&["repo.test"]).self_signed(&key).unwrap();
    let mut corrupted = self_signed.der().as_ref().to_vec();
    *corrupted.last_mut().unwrap() ^= 1;
    assert_eq!(
        verify(&session, &CertificateDer::from(corrupted), &[]).unwrap_err(),
        Error::InvalidCertificate(CertificateError::BadSignature)
    );
    assert!(session.take_pending().unwrap().is_none());
}

#[test]
fn missing_issuer_can_be_approved_despite_unrelated_expired_and_badly_signed_extras() {
    let authority = TestAuthority::new("受信测试 CA");
    let missing = TestAuthority::new("未提供的签发 CA");
    let certificate = missing.issue(leaf_params(&["repo.test"]));
    let mut expired = TestAuthority::new("无关的过期 CA");
    expired.params.not_before = date_time_ymd(2000, 1, 1);
    expired.params.not_after = date_time_ymd(2001, 1, 1);
    let expired_der = expired.params.self_signed(&expired.key).unwrap();
    let unrelated = TestAuthority::new("无关的损坏 CA");
    let mut corrupted = unrelated.certificate.der().as_ref().to_vec();
    *corrupted.last_mut().unwrap() ^= 1;
    let extras = [expired_der.der().clone(), CertificateDer::from(corrupted)];
    let session = session(authority.roots(), None);
    assert!(verify(&session, certificate.certificate.der(), &extras).is_err());
    let prompt = session.take_pending().unwrap().unwrap();
    assert_eq!(prompt.failures, [SvnCertificateFailure::UnknownIssuer]);
    session.trust(&prompt).unwrap();
    verify(&session, certificate.certificate.der(), &extras).unwrap();
}

#[test]
fn connected_issuer_expiration_and_bad_signature_still_prevent_approval() {
    let authority = TestAuthority::new("受信测试 CA");
    let unknown = TestAuthority::new("内部测试 CA");
    let certificate = unknown.issue(leaf_params(&["repo.test"]));
    let session = session(authority.roots(), None);
    let mut expired_params = unknown.params.clone();
    expired_params.not_before = date_time_ymd(2000, 1, 1);
    expired_params.not_after = date_time_ymd(2001, 1, 1);
    let expired = expired_params.self_signed(&unknown.key).unwrap();
    assert!(
        verify(
            &session,
            certificate.certificate.der(),
            &[expired.der().clone()]
        )
        .is_err()
    );
    assert!(session.take_pending().unwrap().is_none());
    let mut corrupted = unknown.certificate.der().as_ref().to_vec();
    *corrupted.last_mut().unwrap() ^= 1;
    assert_eq!(
        verify(
            &session,
            certificate.certificate.der(),
            &[CertificateDer::from(corrupted)]
        )
        .unwrap_err(),
        Error::InvalidCertificate(CertificateError::BadSignature)
    );
    assert!(session.take_pending().unwrap().is_none());
}

#[test]
fn same_named_issuer_candidates_can_use_a_valid_alternative() {
    let authority = TestAuthority::new("受信测试 CA");
    let incorrect = TestAuthority::new("同名内部 CA");
    let correct = TestAuthority::new("同名内部 CA");
    let certificate = correct.issue(leaf_params(&["repo.test"]));
    let session = session(authority.roots(), None);
    let intermediates = [
        incorrect.certificate.der().clone(),
        correct.certificate.der().clone(),
    ];
    assert!(verify(&session, certificate.certificate.der(), &intermediates).is_err());
    let prompt = session.take_pending().unwrap().unwrap();
    session.trust(&prompt).unwrap();
    verify(&session, certificate.certificate.der(), &intermediates).unwrap();
}

#[test]
fn circular_or_excessive_candidate_chains_are_rejected_with_bounded_work() {
    let first = TestAuthority::new("循环 CA A");
    let second = TestAuthority::new("循环 CA B");
    let first_cross_signed = first
        .params
        .signed_by(
            &first.key,
            &Issuer::from_params(&second.params, &second.key),
        )
        .unwrap();
    let second_cross_signed = second
        .params
        .signed_by(&second.key, &Issuer::from_params(&first.params, &first.key))
        .unwrap();
    let leaf = first.issue(leaf_params(&["repo.test"]));
    let parsed = parse_certificate(leaf.certificate.der(), UnixTime::now()).unwrap();
    let cycle = [
        first_cross_signed.der().clone(),
        second_cross_signed.der().clone(),
    ];
    assert!(
        verify_available_chain_signatures(&parsed, &cycle, UnixTime::now())
            .unwrap_err()
            .to_string()
            .contains("循环")
    );
    let too_many = vec![first.certificate.der().clone(); SVN_MAX_CHAIN_CERTIFICATES];
    assert!(
        verify_available_chain_signatures(&parsed, &too_many, UnixTime::now())
            .unwrap_err()
            .to_string()
            .contains("过长")
    );
    let issuer = parse_certificate(first.certificate.der(), UnixTime::now()).unwrap();
    assert!(
        verify_certificate_path(
            0,
            &[&parsed, &issuer],
            UnixTime::now(),
            &mut vec![0],
            &mut 0
        )
        .unwrap_err()
        .to_string()
        .contains("候选过多")
    );
}
