//! SVN 证书确认的应用回归：显式授权、配置持久化、并发弹窗及旧会话隔离。

use super::*;
use crate::config::ConfigManager;
use crate::config::paths::{isolated_test_dir, temporary_test_dir};
use crate::remote::connection::SvnLinkConfig;
use crate::remote::svn_tls::{SvnCertificateFailure, SvnCertificatePrompt};

fn test_app() -> ArgusApp {
    ArgusApp::new_with_config_manager(ConfigManager::new(
        isolated_test_dir("svn-certificate-ui").join("settings.toml"),
    ))
}

fn certificate_prompt() -> SvnCertificatePrompt {
    SvnCertificatePrompt {
        host: "svn.example.test".into(),
        port: 443,
        fingerprint: format!("SHA256:{}", "ab".repeat(32)),
        subject: "CN=svn.example.test".into(),
        issuer: "CN=测试签发机构".into(),
        subject_alt_names: vec!["svn.example.test".into()],
        not_before: "2026-01-01 00:00:00 UTC".into(),
        not_after: "2027-01-01 00:00:00 UTC".into(),
        failures: vec![SvnCertificateFailure::UnknownIssuer],
        previous_fingerprint: None,
    }
}

fn insert_session(app: &mut ArgusApp, session_id: usize) -> usize {
    let link_id = app
        .config
        .connections
        .add_svn_link(
            None,
            &format!("测试 SVN {session_id}"),
            SvnLinkConfig {
                url: "https://svn.example.test/repository".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let link = app.config.connections.link(link_id).unwrap().clone();
    let (sender, _) = std::sync::mpsc::channel();
    let session =
        RemoteFileSessionState::connecting(session_id, &link, RemoteFileBackend::Svn, sender);
    app.remote_file_sessions.insert(session_id, session);
    link_id
}

fn current_prompt(app: &ArgusApp) -> ConnectionSvnCertificatePromptState {
    let Some(ConnectionDialogState::SvnCertificate(prompt)) = &app.connection_dialog else {
        panic!("应显示证书确认弹窗");
    };
    prompt.clone()
}

#[test]
fn temporary_and_permanent_trust_require_explicit_decisions() {
    for decision in [
        SvnCertificateDecision::TrustOnce,
        SvnCertificateDecision::TrustPermanently,
    ] {
        let mut app = test_app();
        let link_id = insert_session(&mut app, 1);
        let (request, receiver) = SvnCertificateApproval::test_request(certificate_prompt());
        app.apply_remote_svn_certificate_event(1, request.clone());
        app.submit_connection_dialog();
        assert!(receiver.try_recv().is_err(), "Enter 不得批准证书");
        assert!(request.is_pending());
        app.respond_svn_certificate_prompt(current_prompt(&app), decision);
        assert_eq!(receiver.try_recv().unwrap(), decision);
        assert_eq!(
            app.remote_file_sessions[&1].status,
            RemoteFileStatus::Connecting
        );
        assert!(app.connection_dialog.is_none());
        assert!(
            app.remote_file_sessions[&1]
                .pending_svn_certificate
                .is_none()
        );
        let persisted = decision == SvnCertificateDecision::TrustPermanently;
        assert_eq!(
            app.config
                .connections
                .trusted_svn_certificate(link_id, "https://svn.example.test/repository")
                .is_some(),
            persisted
        );
        assert_eq!(
            app.config_manager
                .load()
                .connections
                .trusted_svn_certificate(link_id, "https://svn.example.test/repository")
                .is_some(),
            persisted
        );
    }
}

#[test]
fn closing_certificate_dialog_rejects_and_advances_pending_sessions() {
    let mut app = test_app();
    insert_session(&mut app, 1);
    insert_session(&mut app, 2);
    let (first, first_receiver) = SvnCertificateApproval::test_request(certificate_prompt());
    let (second, second_receiver) = SvnCertificateApproval::test_request(certificate_prompt());
    app.apply_remote_svn_certificate_event(1, first);
    app.apply_remote_svn_certificate_event(2, second);
    assert_eq!(current_prompt(&app).session_id, 1);
    app.open_svn_certificate_prompt(2);
    assert_eq!(
        current_prompt(&app).session_id,
        1,
        "查看入口不覆盖尚未处理的弹窗"
    );
    app.close_connection_dialog();
    assert_eq!(
        first_receiver.try_recv().unwrap(),
        SvnCertificateDecision::Reject
    );
    assert_eq!(
        app.remote_file_sessions[&1].status,
        RemoteFileStatus::Failed
    );
    assert_eq!(current_prompt(&app).session_id, 2);
    assert!(second_receiver.try_recv().is_err());
    app.disconnect_remote_file_session(2);
    assert_eq!(
        second_receiver.try_recv().unwrap(),
        SvnCertificateDecision::Reject
    );
    assert!(app.connection_dialog.is_none());
}

#[test]
fn stale_or_expired_dialog_cannot_authorize_a_new_certificate() {
    let mut app = test_app();
    insert_session(&mut app, 1);
    let (first, first_receiver) = SvnCertificateApproval::test_request(certificate_prompt());
    app.apply_remote_svn_certificate_event(1, first);
    let old_dialog = current_prompt(&app);
    let mut changed = certificate_prompt();
    changed.fingerprint = format!("SHA256:{}", "cd".repeat(32));
    let (second, second_receiver) = SvnCertificateApproval::test_request(changed);
    app.apply_remote_svn_certificate_event(1, second.clone());
    assert_eq!(
        first_receiver.try_recv().unwrap(),
        SvnCertificateDecision::Reject
    );
    app.respond_svn_certificate_prompt(old_dialog, SvnCertificateDecision::TrustPermanently);
    assert!(second_receiver.try_recv().is_err());
    assert_eq!(current_prompt(&app).request_id, second.request_id);
    assert!(app.config.connections.trusted_svn_certificates.is_empty());
    let expired_dialog = current_prompt(&app);
    second.respond(SvnCertificateDecision::Reject).unwrap();
    app.respond_svn_certificate_prompt(expired_dialog, SvnCertificateDecision::TrustPermanently);
    assert!(app.config.connections.trusted_svn_certificates.is_empty());
    assert!(app.connection_dialog.is_none());
}

#[test]
fn failed_persistence_leaves_request_unapproved_and_exposes_error() {
    let mut app = test_app();
    insert_session(&mut app, 1);
    let (request, receiver) = SvnCertificateApproval::test_request(certificate_prompt());
    app.apply_remote_svn_certificate_event(1, request.clone());
    let directory = temporary_test_dir("svn-certificate-save-failure");
    let blocked_path = directory.path().join("settings.toml");
    std::fs::create_dir(&blocked_path).unwrap();
    app.config_manager = ConfigManager::new(blocked_path);
    app.respond_svn_certificate_prompt(
        current_prompt(&app),
        SvnCertificateDecision::TrustPermanently,
    );
    assert!(request.is_pending());
    assert!(receiver.try_recv().is_err());
    assert!(app.config.connections.trusted_svn_certificates.is_empty());
    assert!(
        current_prompt(&app)
            .error_message
            .unwrap()
            .contains("保存失败")
    );
    app.respond_svn_certificate_prompt(current_prompt(&app), SvnCertificateDecision::TrustOnce);
    assert_eq!(
        receiver.try_recv().unwrap(),
        SvnCertificateDecision::TrustOnce
    );
}

#[test]
fn vanished_worker_rolls_back_saved_trust_instead_of_reporting_success() {
    let mut app = test_app();
    insert_session(&mut app, 1);
    let (request, receiver) = SvnCertificateApproval::test_request(certificate_prompt());
    app.apply_remote_svn_certificate_event(1, request);
    drop(receiver);
    app.respond_svn_certificate_prompt(
        current_prompt(&app),
        SvnCertificateDecision::TrustPermanently,
    );
    assert!(app.config.connections.trusted_svn_certificates.is_empty());
    assert!(
        app.config_manager
            .load()
            .connections
            .trusted_svn_certificates
            .is_empty()
    );
    assert!(
        current_prompt(&app)
            .error_message
            .unwrap()
            .contains("未生效")
    );
    app.close_connection_dialog();
    assert!(app.connection_dialog.is_none());
}

#[test]
fn changed_endpoint_and_closed_session_do_not_accept_old_certificates() {
    let mut app = test_app();
    let link_id = insert_session(&mut app, 1);
    let (request, receiver) = SvnCertificateApproval::test_request(certificate_prompt());
    app.apply_remote_svn_certificate_event(1, request);
    app.config
        .connections
        .update_svn_link(
            link_id,
            "已更换地址",
            SvnLinkConfig {
                url: "https://other.example.test/repository".into(),
                ..Default::default()
            },
        )
        .unwrap();
    app.respond_svn_certificate_prompt(current_prompt(&app), SvnCertificateDecision::TrustOnce);
    assert!(receiver.try_recv().is_err());
    assert!(current_prompt(&app).error_message.is_some());
    app.disconnect_remote_file_session(1);
    assert_eq!(receiver.try_recv().unwrap(), SvnCertificateDecision::Reject);
    let (late, receiver) = SvnCertificateApproval::test_request(certificate_prompt());
    app.apply_remote_svn_certificate_event(1, late);
    assert_eq!(receiver.try_recv().unwrap(), SvnCertificateDecision::Reject);
    assert!(app.connection_dialog.is_none());
}

#[gpui::test]
fn reconnected_worker_ignores_old_failure_and_certificate_events(cx: &mut gpui::TestAppContext) {
    let app = cx.new(|_| test_app());
    app.update(cx, |app, cx| {
        let link_id = insert_session(app, 1);
        let old_generation = app.remote_file_sessions[&1].worker_generation;
        let link = app.config.connections.link(link_id).unwrap().clone();
        let (sender, _) = std::sync::mpsc::channel();
        app.remote_file_sessions.insert(
            1,
            RemoteFileSessionState::connecting(1, &link, RemoteFileBackend::Svn, sender),
        );
        let (request, receiver) = SvnCertificateApproval::test_request(certificate_prompt());
        app.apply_remote_svn_certificate_event(1, request.clone());
        assert!(!app.apply_remote_file_event_for_generation(
            1,
            old_generation,
            RemoteFileEvent::Failed {
                session_id: 1,
                message: "旧连接已结束".into()
            },
            cx
        ));
        assert_eq!(
            app.remote_file_sessions[&1].status,
            RemoteFileStatus::AwaitingCertificate
        );
        assert!(receiver.try_recv().is_err());
        assert_eq!(current_prompt(app).request_id, request.request_id);
        let current_generation = app.remote_file_sessions[&1].worker_generation;
        assert!(app.apply_remote_file_event_for_generation(
            1,
            current_generation,
            RemoteFileEvent::Failed {
                session_id: 1,
                message: "当前连接已结束".into()
            },
            cx
        ));
        assert_eq!(receiver.try_recv().unwrap(), SvnCertificateDecision::Reject);
        assert!(app.connection_dialog.is_none());
    });
}

#[gpui::test]
fn certificate_failure_during_existing_session_clears_prompt(cx: &mut gpui::TestAppContext) {
    let app = cx.new(|_| test_app());
    app.update(cx, |app, cx| {
        insert_session(app, 1);
        insert_session(app, 2);
        let (first, receiver) = SvnCertificateApproval::test_request(certificate_prompt());
        let (second, _second_receiver) = SvnCertificateApproval::test_request(certificate_prompt());
        app.apply_remote_svn_certificate_event(1, first);
        app.apply_remote_svn_certificate_event(2, second);
        app.apply_remote_file_event(
            RemoteFileEvent::OperationFailed {
                session_id: 1,
                message: "等待 SVN 证书确认超时，请重新连接".into(),
            },
            cx,
        );
        assert_eq!(receiver.try_recv().unwrap(), SvnCertificateDecision::Reject);
        assert!(
            app.remote_file_sessions[&1]
                .pending_svn_certificate
                .is_none()
        );
        assert_eq!(current_prompt(app).session_id, 2);
        assert_eq!(
            app.remote_file_sessions[&1].status,
            RemoteFileStatus::Connected
        );
    });
}

#[gpui::test]
fn certificate_modal_keeps_all_decisions_visible_and_waiting_page_can_reopen(
    cx: &mut gpui::TestAppContext,
) {
    let (app, cx) = cx.add_window_view(|_, _| test_app());
    let receiver = app.update(cx, |app, cx| {
        insert_session(app, 1);
        let mut prompt = certificate_prompt();
        prompt.subject_alt_names = (0..20)
            .map(|index| format!("server-{index}.example.test"))
            .collect();
        prompt.previous_fingerprint = Some(format!("SHA256:{}", "cd".repeat(32)));
        let (request, receiver) = SvnCertificateApproval::test_request(prompt);
        app.apply_remote_svn_certificate_event(1, request);
        app.workspace = Workspace::Connections;
        app.create_remote_file_tab_for_session(1);
        cx.notify();
        receiver
    });
    cx.simulate_resize(gpui::size(gpui::px(1400.0), gpui::px(900.0)));
    cx.simulate_keystrokes("enter");
    assert!(receiver.try_recv().is_err(), "真实 Enter 按键不得批准证书");
    let container = cx
        .debug_bounds("connection-svn-certificate-dialog-container-shell")
        .unwrap();
    // 弹窗根节点自带 1px 边框，内容整体向内缩 1px，从外壳边界量到按钮应为 19 + 1。
    let bordered_bottom_padding = crate::ui::components::modal_dialog::MODAL_BOTTOM_PADDING + 1.0;
    for id in [
        "connection-svn-certificate-reject",
        "connection-svn-certificate-trust-once",
        "connection-svn-certificate-trust-permanently",
    ] {
        let button = cx.debug_bounds(id).unwrap();
        assert!(button.top() >= container.top());
        assert_eq!(
            f32::from(container.bottom() - button.bottom()),
            bordered_bottom_padding
        );
    }
    let blocked_directory = temporary_test_dir("svn-certificate-modal-save-failure");
    let blocked_path = blocked_directory.path().join("settings.toml");
    std::fs::create_dir(&blocked_path).unwrap();
    app.update(cx, |app, _| {
        app.config_manager = ConfigManager::new(blocked_path)
    });
    let permanent_button = cx
        .debug_bounds("connection-svn-certificate-trust-permanently")
        .unwrap();
    cx.simulate_click(permanent_button.center(), gpui::Modifiers::default());
    let error = cx
        .debug_bounds("connection-svn-certificate-error")
        .expect("保存错误应固定显示在底栏");
    let scroll = cx
        .debug_bounds("connection-svn-certificate-dialog-scroll")
        .unwrap();
    let permanent_button = cx
        .debug_bounds("connection-svn-certificate-trust-permanently")
        .unwrap();
    assert!(
        error.top() >= scroll.bottom(),
        "长证书正文不能把保存错误藏入滚动区域"
    );
    assert!(error.bottom() <= permanent_button.top());
    assert_eq!(
        f32::from(container.bottom() - permanent_button.bottom()),
        bordered_bottom_padding
    );
    assert!(receiver.try_recv().is_err(), "保存失败不得发送证书批准");
    app.update(cx, |app, cx| {
        app.connection_dialog = None;
        cx.notify();
    });
    cx.simulate_resize(gpui::size(gpui::px(1401.0), gpui::px(900.0)));
    let reopen = cx.debug_bounds("remote-file-show-certificate").unwrap();
    cx.simulate_click(reopen.center(), gpui::Modifiers::default());
    assert!(
        cx.debug_bounds("connection-svn-certificate-reject")
            .is_some()
    );
    assert!(receiver.try_recv().is_err());
    cx.simulate_keystrokes("escape");
    assert_eq!(receiver.try_recv().unwrap(), SvnCertificateDecision::Reject);
}
