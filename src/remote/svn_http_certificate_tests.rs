//! SVN HTTPS 真实传输回归测试：临时生成证书，只访问本机临时端口，不保存私钥或证书文件。

use super::*;

use std::io::Write as _;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use rcgen::{CertificateParams, ExtendedKeyUsagePurpose, KeyPair};
use rustls::pki_types::PrivatePkcs8KeyDer;
use rustls::{ServerConfig, ServerConnection, StreamOwned};

use crate::remote::remote_file::RemoteFileEvent;
use crate::remote::svn_certificate::{SvnCertificateApproval, SvnCertificateDecision};
use crate::remote::svn_tls::SvnCertificateFailure;
use crate::remote::worker::WorkerGuard;

const TEST_WAIT: Duration = Duration::from_secs(10);
const TEST_SOCKET_TIMEOUT: Duration = Duration::from_secs(2);
const TEST_SERVER_LIFETIME: Duration = Duration::from_secs(30);
const TEST_POLL_INTERVAL: Duration = Duration::from_millis(10);
const TEST_MAX_REQUEST_BYTES: usize = 64 * 1024;

/// 只记录请求方法与是否带鉴权头，不在断言或通道中复制凭据原文。
#[derive(Debug, Eq, PartialEq)]
enum ServerObservation {
    HandshakeRejected,
    HttpRequest {
        method: String,
        has_authorization: bool,
    },
}

struct TestSvnHttpsServer {
    address: SocketAddr,
    observations: mpsc::Receiver<ServerObservation>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<()>>>,
}

impl TestSvnHttpsServer {
    /// 每个测试拥有独立证书和临时端口，服务线程的接入、握手和读写均有时间上限。
    fn start() -> Self {
        let mut params = CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap();
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let signing_key = KeyPair::generate().unwrap();
        let certificate = params.self_signed(&signing_key).unwrap();
        let tls = Arc::new(
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    vec![certificate.der().clone()],
                    PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
                )
                .unwrap(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (sender, observations) = mpsc::channel();
        let worker = thread::spawn(move || {
            let started = Instant::now();
            while !worker_stop.load(Ordering::Acquire) {
                if started.elapsed() >= TEST_SERVER_LIFETIME {
                    bail!("SVN HTTPS 测试服务未在限定时间内结束");
                }
                match listener.accept() {
                    Ok((socket, _)) => {
                        let observation = serve_connection(socket, tls.clone())?;
                        sender.send(observation).context("测试观察通道已关闭")?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(TEST_POLL_INTERVAL);
                    }
                    Err(error) => return Err(error).context("测试 TLS 服务接入失败"),
                }
            }
            Ok(())
        });
        Self {
            address,
            observations,
            stop,
            worker: Some(worker),
        }
    }

    /// 测试凭据运行时生成，仅用于证明鉴权请求在证书批准之后才进入服务端。
    fn link_config(&self) -> SvnLinkConfig {
        SvnLinkConfig {
            url: format!("https://{}/repo", self.address),
            username: Some("reader".to_owned()),
            password: Some(uuid::Uuid::new_v4().to_string()),
            ..Default::default()
        }
    }

    fn next_observation(&self) -> ServerObservation {
        self.observations
            .recv_timeout(TEST_WAIT)
            .expect("测试服务应在限定时间内收到握手或 HTTP 请求")
    }

    fn finish(mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker
            .take()
            .unwrap()
            .join()
            .expect("测试 TLS 服务线程不能崩溃")
            .expect("测试 TLS 服务不能发生未处理错误");
    }
}

impl Drop for TestSvnHttpsServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            match worker.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => eprintln!("关闭 SVN HTTPS 测试服务失败：{error:#}"),
                Err(_) => eprintln!("关闭 SVN HTTPS 测试服务时线程已崩溃"),
            }
        }
    }
}

/// 完整握手之前不读取 HTTP，未知证书被拒绝时应仅收到 TLS alert。
fn serve_connection(mut socket: TcpStream, config: Arc<ServerConfig>) -> Result<ServerObservation> {
    socket.set_read_timeout(Some(TEST_SOCKET_TIMEOUT))?;
    socket.set_write_timeout(Some(TEST_SOCKET_TIMEOUT))?;
    let mut connection = ServerConnection::new(config)?;
    while connection.is_handshaking() {
        if let Err(error) = connection.complete_io(&mut socket) {
            if error.kind() == std::io::ErrorKind::InvalidData {
                return Ok(ServerObservation::HandshakeRejected);
            }
            return Err(error).context("测试 TLS 握手未完成");
        }
    }
    let mut stream = StreamOwned::new(connection, socket);
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        if headers.len() >= TEST_MAX_REQUEST_BYTES {
            bail!("测试 HTTP 请求头超过上限");
        }
        let mut byte = [0_u8];
        stream
            .read_exact(&mut byte)
            .context("读取测试 HTTP 请求头失败")?;
        headers.push(byte[0]);
    }
    let headers = std::str::from_utf8(&headers).context("测试 HTTP 请求头编码无效")?;
    let method = headers
        .split_whitespace()
        .next()
        .context("测试 HTTP 请求方法缺失")?
        .to_owned();
    let has_authorization = headers
        .lines()
        .any(|line| line.to_ascii_lowercase().starts_with("authorization:"));
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then_some(value.trim())
        })
        .map(str::parse::<usize>)
        .transpose()?
        .unwrap_or(0);
    if content_length > TEST_MAX_REQUEST_BYTES {
        bail!("测试 HTTP 请求正文超过上限");
    }
    let mut body = vec![0_u8; content_length];
    stream
        .read_exact(&mut body)
        .context("读取测试 HTTP 请求正文失败")?;
    let response = match method.as_str() {
        "OPTIONS" => "HTTP/1.1 200 OK\r\nSVN-Youngest-Rev: 42\r\nSVN-Repository-Root: /repo\r\nSVN-Rev-Root-Stub: /repo/!svn/rvr\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
        "PROPFIND" => {
            let body = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
<D:response><D:href>/repo/!svn/rvr/42/hello.txt</D:href><D:propstat><D:prop><D:displayname>hello.txt</D:displayname><D:resourcetype/><D:getcontentlength>6</D:getcontentlength></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
</D:multistatus>"#;
            format!("HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
        }
        _ => bail!("收到非预期的测试 HTTP 方法"),
    };
    stream
        .write_all(response.as_bytes())
        .context("写入测试 HTTP 响应失败")?;
    stream.flush().context("刷新测试 HTTP 响应失败")?;
    Ok(ServerObservation::HttpRequest {
        method,
        has_authorization,
    })
}

/// 使用短轮询保证事件缺失也会结束测试，不依赖无界阻塞接收。
fn receive_approval(receiver: &async_channel::Receiver<RemoteFileEvent>) -> SvnCertificateApproval {
    let started = Instant::now();
    loop {
        match receiver.try_recv() {
            Ok(RemoteFileEvent::SvnCertificateVerification {
                session_id,
                request,
            }) => {
                assert_eq!(session_id, 1);
                return request;
            }
            Ok(_) => panic!("预期收到 SVN 证书确认事件"),
            Err(async_channel::TryRecvError::Empty) if started.elapsed() < TEST_WAIT => {
                thread::sleep(TEST_POLL_INTERVAL);
            }
            Err(error) => panic!("未在限定时间内收到 SVN 证书确认事件：{error}"),
        }
    }
}

fn trust_from_approval(request: &SvnCertificateApproval) -> SvnCertificateTrust {
    SvnCertificateTrust {
        host: request.prompt.host.clone(),
        port: request.prompt.port,
        fingerprint: request.prompt.fingerprint.clone(),
        allowed_failures: request.prompt.failures.clone(),
    }
}

fn assert_http_request(server: &TestSvnHttpsServer, method: &str) {
    assert_eq!(
        server.next_observation(),
        ServerObservation::HttpRequest {
            method: method.to_owned(),
            has_authorization: true,
        }
    );
}

/// 真实 ureq 握手先进入审批，批准后重试 OPTIONS；后续新握手沿用本次信任读取目录。
#[test]
fn https_certificate_approval_retries_discovery_and_reads_directory() {
    let server = TestSvnHttpsServer::start();
    let config = server.link_config();
    let (_guard, cancellation) = WorkerGuard::new(1);
    let (event_sender, events) = async_channel::bounded(4);
    let (result_sender, results) = mpsc::channel();
    let client = thread::spawn(move || {
        let result = (|| {
            let mut session = HttpSvnSession::open(
                &config,
                None,
                Some(SvnCertificateContext {
                    session_id: 1,
                    event_sender,
                    cancellation,
                }),
            )?;
            assert_eq!(session.latest_revision()?, 42);
            let entries = session.list_directory("", 42)?;
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].name, "hello.txt");
            Ok::<_, anyhow::Error>(())
        })();
        result_sender.send(result).expect("测试结果通道应保持打开");
    });
    let request = receive_approval(&events);
    assert_eq!(
        request.prompt.failures,
        [SvnCertificateFailure::UnknownIssuer]
    );
    assert_eq!(
        server.next_observation(),
        ServerObservation::HandshakeRejected
    );
    assert!(server.observations.try_recv().is_err());
    request.respond(SvnCertificateDecision::TrustOnce).unwrap();
    results.recv_timeout(TEST_WAIT).unwrap().unwrap();
    client.join().unwrap();
    assert_http_request(&server, "OPTIONS");
    assert_http_request(&server, "PROPFIND");
    assert!(events.try_recv().is_err());
    server.finish();
}

/// 用户拒绝后客户端终止，不得将含鉴权头的任何 HTTP 字节发送给服务器。
#[test]
fn https_rejected_certificate_never_sends_http_request() {
    let server = TestSvnHttpsServer::start();
    let config = server.link_config();
    let (_guard, cancellation) = WorkerGuard::new(1);
    let (event_sender, events) = async_channel::bounded(4);
    let (result_sender, results) = mpsc::channel();
    let client = thread::spawn(move || {
        let result = HttpSvnSession::open(
            &config,
            None,
            Some(SvnCertificateContext {
                session_id: 1,
                event_sender,
                cancellation,
            }),
        )
        .map(|_| ());
        result_sender.send(result).expect("测试结果通道应保持打开");
    });
    let request = receive_approval(&events);
    assert_eq!(
        server.next_observation(),
        ServerObservation::HandshakeRejected
    );
    request.respond(SvnCertificateDecision::Reject).unwrap();
    let error = results.recv_timeout(TEST_WAIT).unwrap().unwrap_err();
    assert!(error.to_string().contains("已拒绝 SVN 服务器证书"));
    client.join().unwrap();
    assert!(server.observations.try_recv().is_err());
    server.finish();
}

/// 将首次确认的公开指纹交给新会话，重连成功且不会再次弹出相同证书的审批请求。
#[test]
fn https_saved_certificate_pin_reconnects_without_approval() {
    let server = TestSvnHttpsServer::start();
    let config = server.link_config();
    let first_config = config.clone();
    let (_guard, cancellation) = WorkerGuard::new(1);
    let (event_sender, events) = async_channel::bounded(4);
    let (result_sender, results) = mpsc::channel();
    let client = thread::spawn(move || {
        let result = HttpSvnSession::open(
            &first_config,
            None,
            Some(SvnCertificateContext {
                session_id: 1,
                event_sender,
                cancellation,
            }),
        )
        .map(|_| ());
        result_sender.send(result).expect("测试结果通道应保持打开");
    });
    let request = receive_approval(&events);
    let trusted = trust_from_approval(&request);
    assert_eq!(
        server.next_observation(),
        ServerObservation::HandshakeRejected
    );
    request
        .respond(SvnCertificateDecision::TrustPermanently)
        .unwrap();
    results.recv_timeout(TEST_WAIT).unwrap().unwrap();
    client.join().unwrap();
    assert_http_request(&server, "OPTIONS");

    // 没有审批上下文时，任何未命中信任记录的握手都会直接失败，避免测试意外等待弹窗。
    let mut reconnected = HttpSvnSession::open(&config, Some(trusted), None).unwrap();
    assert_eq!(reconnected.latest_revision().unwrap(), 42);
    assert_http_request(&server, "OPTIONS");
    assert!(events.try_recv().is_err());
    server.finish();
}
