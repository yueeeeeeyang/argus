//! 文件职责：验证仓库文件版本对比事件、历史弹窗和预览自动重试流程。

use super::*;
use crate::config::ConfigManager;
use crate::config::paths::isolated_test_dir;
use crate::remote::connection::SvnLinkConfig;
use crate::remote::remote_file::{
    RepositoryHistoryEntry, RepositoryVersion, RepositoryVersionKind,
};
use crate::ui::file_diff_window::FileDiffWindow;

/// 构造隔离真实用户目录的应用状态。
fn test_app() -> ArgusApp {
    ArgusApp::new_with_config_manager(ConfigManager::new(
        isolated_test_dir("svn-file-diff-ui").join("settings.toml"),
    ))
}

/// 插入处于读取状态的 SVN 会话，并返回当前 worker 代次。
fn insert_reading_svn_session(app: &mut ArgusApp, session_id: usize) -> uuid::Uuid {
    let link_id = app
        .config
        .connections
        .add_svn_link(
            None,
            "测试 SVN 仓库",
            SvnLinkConfig {
                url: "https://svn.example.test/repository".to_string(),
                ..Default::default()
            },
        )
        .expect("应能创建测试 SVN 链接");
    let link = app.config.connections.link(link_id).unwrap().clone();
    let (sender, _receiver) = std::sync::mpsc::channel();
    let mut session =
        RemoteFileSessionState::connecting(session_id, &link, RemoteFileBackend::Svn, sender);
    session.status = RemoteFileStatus::Transferring;
    let generation = session.worker_generation;
    app.remote_file_sessions.insert(session_id, session);
    generation
}

/// 验证对比结果事件恢复会话状态并打开独立的 diff 窗口。
#[gpui::test]
fn comparison_event_opens_independent_diff_window(cx: &mut gpui::TestAppContext) {
    let app = cx.new(|_| test_app());
    let windows_before = cx.windows().len();

    app.update(cx, |app, cx| {
        let generation = insert_reading_svn_session(app, 1);
        assert!(app.apply_remote_file_event_for_generation(
            1,
            generation,
            RemoteFileEvent::FileComparisonLoaded {
                session_id: 1,
                file_name: "app.rs".to_string(),
                current_version: "HEAD (r42)".to_string(),
                target_version: "r7".to_string(),
                current_content: FilePreviewContent::Text {
                    content: "old\n".to_string(),
                    truncated: false,
                },
                target_content: FilePreviewContent::Text {
                    content: "new\n".to_string(),
                    truncated: false,
                },
            },
            cx,
        ));

        assert_eq!(
            app.remote_file_sessions[&1].status,
            RemoteFileStatus::Connected
        );
        assert_eq!(
            app.remote_file_sessions[&1].message.as_deref(),
            Some("已加载版本对比：app.rs")
        );
        assert!(app.placeholder_notice.contains("已打开版本对比"));
    });

    let windows = cx.windows();
    assert_eq!(windows.len(), windows_before + 1);
    assert!(
        windows
            .iter()
            .any(|handle| handle.downcast::<FileDiffWindow>().is_some()),
        "应打开 FileDiffWindow 独立窗口"
    );
}

/// 插入带单个可预览 SVN 文件的已连接会话，并返回命令接收端。
fn insert_preview_svn_session(
    app: &mut ArgusApp,
    session_id: usize,
) -> std::sync::mpsc::Receiver<RemoteFileCommand> {
    let link_id = app
        .config
        .connections
        .add_svn_link(
            None,
            "测试 SVN 预览仓库",
            SvnLinkConfig {
                url: "https://svn.example.test/repository".to_string(),
                ..Default::default()
            },
        )
        .expect("应能创建测试 SVN 链接");
    let link = app.config.connections.link(link_id).unwrap().clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut session =
        RemoteFileSessionState::connecting(session_id, &link, RemoteFileBackend::Svn, sender);
    session.status = RemoteFileStatus::Connected;
    session.repository_versions = vec![RepositoryVersion {
        id: "HEAD".to_string(),
        label: "HEAD (r42)".to_string(),
        kind: RepositoryVersionKind::SvnHead,
    }];
    session.selected_repository_version = Some("HEAD".to_string());
    session.entries = vec![RemoteFileEntry {
        name: "app.rs".to_string(),
        path: "/app.rs".to_string(),
        kind: RemoteFileEntryKind::RegularFile,
        size: Some(256),
        mtime: None,
        permissions: Some(0o100644),
    }];
    session.selected_paths.insert("/app.rs".to_string());
    app.remote_file_sessions.insert(session_id, session);
    receiver
}

/// 接收一次预览命令并返回请求 ID。
fn recv_preview_request(receiver: &std::sync::mpsc::Receiver<RemoteFileCommand>) -> u64 {
    match receiver.recv_timeout(std::time::Duration::from_secs(1)) {
        Ok(RemoteFileCommand::ReadFileContent { request_id, .. }) => request_id,
        other => panic!("应收到预览读取命令，实际为 {other:?}"),
    }
}

/// 验证预览失败后自动重试三次，最终停止并允许手动重试。
#[gpui::test]
fn preview_failure_auto_retries_three_times_then_allows_manual_retry(
    cx: &mut gpui::TestAppContext,
) {
    let app = cx.new(|_| test_app());
    let mut receiver = None;
    app.update(cx, |app, cx| {
        receiver = Some(insert_preview_svn_session(app, 1));
        app.request_remote_file_preview(1, "/app.rs".to_string(), Some(256), cx);
        assert!(app.remote_file_previews.contains_key(&1));
        assert!(app.remote_file_previews[&1].is_loading);
    });
    let receiver = receiver.expect("应创建预览命令接收端");
    let request_id = 1;
    assert_eq!(recv_preview_request(&receiver), request_id);

    for expected_retry in 1..=FILE_PREVIEW_AUTO_RETRY_LIMIT {
        app.update(cx, |app, cx| {
            app.apply_remote_file_event(
                RemoteFileEvent::FileContentLoaded {
                    session_id: 1,
                    request_id,
                    file_name: "app.rs".to_string(),
                    content: FilePreviewContent::Error("临时网络错误".to_string()),
                },
                cx,
            );
        });
        cx.executor().advance_clock(REMOTE_FILE_PREVIEW_RETRY_DELAY);
        cx.run_until_parked();

        let (retry_count, is_loading) = app.read_with(cx, |app, _| {
            let state = &app.remote_file_previews[&1];
            (state.retry_count, state.is_loading)
        });
        assert_eq!(retry_count, expected_retry);
        assert!(is_loading);
        // 每次失败后都会再发一次同请求 ID 的读取命令。
        assert_eq!(recv_preview_request(&receiver), request_id);
    }

    // 第 4 次失败发生在 3 次自动重试之后，此时停止并展示手动重试。
    app.update(cx, |app, cx| {
        app.apply_remote_file_event(
            RemoteFileEvent::FileContentLoaded {
                session_id: 1,
                request_id,
                file_name: "app.rs".to_string(),
                content: FilePreviewContent::Error("最终失败".to_string()),
            },
            cx,
        );
    });
    cx.run_until_parked();
    assert!(!app.read_with(cx, |app, _| { app.remote_file_previews[&1].is_loading }));
    assert!(receiver.try_recv().is_err());

    // 手动重试重置计数并重新发送同一请求。
    app.update(cx, |app, cx| app.retry_remote_file_preview(1, cx));
    let (retry_count, is_loading) = app.read_with(cx, |app, _| {
        let state = &app.remote_file_previews[&1];
        (state.retry_count, state.is_loading)
    });
    assert_eq!(retry_count, 0);
    assert!(is_loading);
    assert_eq!(recv_preview_request(&receiver), request_id);
}

/// 验证 SVN 历史事件回填对比弹窗并预选最新提交。
#[gpui::test]
fn repository_history_event_populates_compare_dialog(cx: &mut gpui::TestAppContext) {
    let app = cx.new(|_| test_app());
    let mut receiver = None;
    app.update(cx, |app, _| {
        receiver = Some(insert_preview_svn_session(app, 1));
        app.open_repository_file_diff_dialog(1);
    });
    let receiver = receiver.expect("应创建命令接收端");
    assert!(matches!(
        receiver.try_recv(),
        Ok(RemoteFileCommand::LoadFileHistory { remote_path })
            if remote_path == "/app.rs"
    ));

    app.update(cx, |app, cx| {
        app.apply_remote_file_event(
            RemoteFileEvent::FileHistoryLoaded {
                session_id: 1,
                remote_path: "/app.rs".to_string(),
                entries: vec![
                    RepositoryHistoryEntry {
                        target_id: "r42".to_string(),
                        label: "r42".to_string(),
                        author: Some("alice".to_string()),
                        date: Some(1_700_000_000),
                        message: "最新提交".to_string(),
                    },
                    RepositoryHistoryEntry {
                        target_id: "r17".to_string(),
                        label: "r17".to_string(),
                        author: None,
                        date: None,
                        message: "旧提交".to_string(),
                    },
                ],
                error: None,
            },
            cx,
        );
    });

    let dialog = app.read_with(cx, |app, _| match app.remote_file_dialog.as_ref() {
        Some(RemoteFileDialogState::RepositoryDiff(dialog)) => dialog.clone(),
        other => panic!("应保留 SVN 对比弹窗，实际为 {other:?}"),
    });
    assert!(!dialog.is_loading_history);
    assert_eq!(dialog.entries.len(), 2);
    assert_eq!(dialog.selected_target.as_deref(), Some("r42"));
    assert!(dialog.history_error.is_none());
    assert!(app.read_with(cx, |app, _| {
        app.remote_file_sessions[&1].status == RemoteFileStatus::Connected
    }));
}

/// 验证预览读取成功后清理重试状态并保留窗口内容。
#[gpui::test]
fn preview_success_clears_pending_state(cx: &mut gpui::TestAppContext) {
    let app = cx.new(|_| test_app());
    let mut receiver = None;
    app.update(cx, |app, cx| {
        receiver = Some(insert_preview_svn_session(app, 1));
        app.request_remote_file_preview(1, "/app.rs".to_string(), Some(256), cx);
    });
    let receiver = receiver.expect("应创建预览命令接收端");
    let request_id = recv_preview_request(&receiver);

    app.update(cx, |app, cx| {
        app.apply_remote_file_event(
            RemoteFileEvent::FileContentLoaded {
                session_id: 1,
                request_id,
                file_name: "app.rs".to_string(),
                content: FilePreviewContent::Text {
                    content: "fn main() {}\n".to_string(),
                    truncated: false,
                },
            },
            cx,
        );
    });

    assert!(!app.read_with(cx, |app, _| app.remote_file_previews.contains_key(&1)));
    assert!(app.read_with(cx, |app, _| {
        app.remote_file_sessions[&1].status == RemoteFileStatus::Connected
    }));
    assert!(app.read_with(cx, |app, _| {
        app.remote_file_sessions[&1]
            .message
            .as_deref()
            .is_some_and(|message| message.contains("已加载预览"))
    }));
}
