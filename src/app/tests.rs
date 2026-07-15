//! 文件职责：app 模块的单元测试。
//! 创建日期：2026-07-08
//! 作者：Argus 开发团队
//! 主要功能：测试应用状态、来源树、标签页、搜索、分析和连接等核心行为。

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// 测试配置路径计数器，保证每个应用状态使用独立 settings.toml。
static NEXT_TEST_CONFIG_ID: AtomicUsize = AtomicUsize::new(0);

/// 构造隔离真实用户目录的配置管理器。
fn isolated_config_manager() -> ConfigManager {
    let id = NEXT_TEST_CONFIG_ID.fetch_add(1, Ordering::Relaxed);
    let config_dir =
        std::env::temp_dir().join(format!("argus-app-test-{}-{id}", std::process::id()));
    let _ = std::fs::remove_dir_all(&config_dir);
    ConfigManager::new(config_dir.join("settings.toml"))
}

/// 构造使用临时配置路径的应用状态，避免测试污染 `~/.argus`。
fn test_app() -> ArgusApp {
    ArgusApp::new_with_config_manager(isolated_config_manager())
}

/// 在配置中创建一个测试 SSH 链接。
fn add_test_ssh_link(app: &mut ArgusApp) -> ConnectionNodeId {
    app.config
        .connections
        .add_ssh_link(
            None,
            "测试服务器",
            crate::remote::connection::SshLinkConfig {
                host: "127.0.0.1".to_string(),
                port: 22,
                username: "tester".to_string(),
                password: "secret".to_string(),
                private_key_path: None,
                private_key_passphrase: None,
            },
        )
        .expect("应能创建测试 SSH 链接")
}

/// 插入不连接真实服务器的终端会话。
fn insert_test_terminal_session(app: &mut ArgusApp, session_id: usize, link_id: ConnectionNodeId) {
    let link = app
        .config
        .connections
        .link(link_id)
        .expect("应存在测试链接")
        .clone();
    let (sender, _) = std::sync::mpsc::channel();
    let mut session =
        crate::remote::terminal::TerminalSessionState::connecting(session_id, &link, sender);
    session.status = crate::remote::terminal::TerminalStatus::Connected;
    app.terminal_sessions.insert(session_id, session);
}

/// 插入不连接真实服务器的 SFTP 会话，并返回命令接收端。
fn insert_test_sftp_session(
    app: &mut ArgusApp,
    session_id: usize,
    link_id: ConnectionNodeId,
) -> std::sync::mpsc::Receiver<crate::remote::sftp::SftpCommand> {
    let link = app
        .config
        .connections
        .link(link_id)
        .expect("应存在测试链接")
        .clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut session = crate::remote::sftp::SftpSessionState::connecting(
        session_id,
        &link,
        crate::remote::sftp::RemoteFileBackend::Sftp,
        sender,
    );
    session.status = crate::remote::sftp::SftpStatus::Connected;
    session.current_dir = "/home/tester".to_string();
    session.address_input = TextInputState::from_value(session.current_dir.clone());
    app.sftp_sessions.insert(session_id, session);
    receiver
}

/// 日志阅读区展示文本会把制表符固定展开为 4 个空格。
#[test]
fn log_display_text_expands_tab_to_four_spaces() {
    assert_eq!(log_viewer_display_text("a\tb").as_ref(), "a    b");
    assert_eq!(
        log_viewer_display_text("\tlevel\tmessage").as_ref(),
        "    level    message"
    );
}

/// 验证链接工作区侧栏默认使用最小宽度，且拖拽不会污染日志来源侧栏宽度。
#[test]
fn connection_source_panel_width_is_independent_from_log_width() {
    let mut app = test_app();

    assert_eq!(app.current_source_panel_width(), SOURCE_PANEL_DEFAULT_WIDTH);

    app.workspace = Workspace::Connections;
    assert_eq!(app.current_source_panel_width(), SOURCE_PANEL_MIN_WIDTH);

    app.begin_source_panel_resize(0.0);
    assert!(app.resize_source_panel(100.0));
    assert_eq!(
        app.connection_source_panel_width,
        SOURCE_PANEL_MIN_WIDTH + 100.0
    );
    assert_eq!(app.source_panel_width, SOURCE_PANEL_DEFAULT_WIDTH);

    app.workspace = Workspace::LogAnalysis;
    assert_eq!(app.current_source_panel_width(), SOURCE_PANEL_DEFAULT_WIDTH);
}

/// 构造带样例来源树的应用状态，避免单元测试依赖正式启动空态。
fn app_with_placeholder_sources() -> ArgusApp {
    let mut app = test_app();
    app.source_registry = placeholder_source_registry();
    app
}

/// 按当前可见索引返回节点名称，便于验证来源树过滤结果。
fn visible_labels(app: &ArgusApp) -> Vec<String> {
    app.visible_source_ids()
        .iter()
        .filter_map(|source_id| app.source_registry.node(*source_id))
        .map(|source| source.label.clone())
        .collect()
}

/// 按名称查找测试来源 ID，避免测试依赖硬编码数字 ID。
fn source_id_by_label(app: &ArgusApp, label: &str) -> SourceId {
    app.source_registry
        .tree_order_source_ids()
        .iter()
        .copied()
        .find(|source_id| {
            app.source_registry
                .node(*source_id)
                .map(|source| source.label == label)
                .unwrap_or(false)
        })
        .expect("测试样例来源应存在")
}

/// 构造一个已加载的压缩包内目录，模拟用户在压缩包树上直接右键目录。
fn app_with_loaded_archive_directory() -> (ArgusApp, SourceId, SourceId, SourceId) {
    let mut app = test_app();
    let mut registry = SourceRegistry::new();
    let archive_format = crate::loader::archive::ArchiveFormat::Zip;
    let archive_path = PathBuf::from("runtime.zip");
    let dir_id = registry.allocate_id();
    registry.insert_node(SourceTreeNode {
        id: dir_id,
        parent_id: None,
        depth: 0,
        label: "runtime".to_string(),
        kind: SourceKind::ArchiveDirectory,
        location: SourceLocation::ArchiveEntry {
            archive_path: archive_path.clone(),
            root_format: archive_format,
            container_entries: Vec::new(),
            entry_path: "runtime".to_string(),
            format: archive_format,
            archive_depth: 0,
        },
        metadata: SourceMetadata {
            size: None,
            children_loaded: true,
            is_loading: false,
            message: None,
        },
        selected: false,
        expanded: true,
    });

    let first_log_id = registry.allocate_id();
    registry.insert_node(SourceTreeNode {
        id: first_log_id,
        parent_id: Some(dir_id),
        depth: 1,
        label: "thread0100.log".to_string(),
        kind: SourceKind::ArchiveFile,
        location: SourceLocation::ArchiveEntry {
            archive_path: archive_path.clone(),
            root_format: archive_format,
            container_entries: Vec::new(),
            entry_path: "runtime/thread0100.log".to_string(),
            format: archive_format,
            archive_depth: 0,
        },
        metadata: SourceMetadata {
            size: Some(128),
            children_loaded: true,
            is_loading: false,
            message: None,
        },
        selected: false,
        expanded: false,
    });

    let second_log_id = registry.allocate_id();
    registry.insert_node(SourceTreeNode {
        id: second_log_id,
        parent_id: Some(dir_id),
        depth: 1,
        label: "thread0200.log".to_string(),
        kind: SourceKind::ArchiveFile,
        location: SourceLocation::ArchiveEntry {
            archive_path,
            root_format: archive_format,
            container_entries: Vec::new(),
            entry_path: "runtime/thread0200.log".to_string(),
            format: archive_format,
            archive_depth: 0,
        },
        metadata: SourceMetadata {
            size: Some(256),
            children_loaded: true,
            is_loading: false,
            message: None,
        },
        selected: false,
        expanded: false,
    });

    registry.rebuild_all_indices();
    app.source_registry = registry;
    (app, dir_id, first_log_id, second_log_id)
}

/// 验证来源树右键菜单对日志候选和本地目录节点展示 Jstack 与 Runtime 分析入口。
#[test]
fn source_tree_context_menu_shows_analysis_actions_for_supported_sources() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let logs_dir_id = source_id_by_label(&app, "logs");

    app.open_source_tree_context_menu(app_log_id, gpui::point(gpui::px(1.0), gpui::px(1.0)));

    assert!(matches!(
        app.active_menu.as_ref().map(|menu| &menu.kind),
        Some(ActiveMenuKind::SourceTree { source_id }) if *source_id == app_log_id
    ));
    assert_eq!(app.active_menu_entries().len(), 2);
    assert!(matches!(
        app.active_menu_entries()[0].action,
        MenuAction::OpenJstackAnalysis { source_id } if source_id == app_log_id
    ));
    assert!(matches!(
        app.active_menu_entries()[1].action,
        MenuAction::OpenRuntimeAnalysis { source_id } if source_id == app_log_id
    ));

    app.close_active_menu();
    app.open_source_tree_context_menu(logs_dir_id, gpui::point(gpui::px(1.0), gpui::px(1.0)));

    assert!(matches!(
        app.active_menu.as_ref().map(|menu| &menu.kind),
        Some(ActiveMenuKind::SourceTree { source_id }) if *source_id == logs_dir_id
    ));
    assert_eq!(app.active_menu_entries().len(), 2);
    assert!(matches!(
        app.active_menu_entries()[0].action,
        MenuAction::OpenJstackAnalysis { source_id } if source_id == logs_dir_id
    ));
    assert!(matches!(
        app.active_menu_entries()[1].action,
        MenuAction::OpenRuntimeAnalysis { source_id } if source_id == logs_dir_id
    ));
}

/// 验证 SSH 终端正文右键菜单展示文件管理入口。
#[test]
fn terminal_context_menu_shows_file_manager_action() {
    let mut app = test_app();
    let link_id = add_test_ssh_link(&mut app);
    insert_test_terminal_session(&mut app, 7, link_id);

    app.open_terminal_context_menu(7, gpui::point(gpui::px(1.0), gpui::px(1.0)));

    assert!(matches!(
        app.active_menu.as_ref().map(|menu| &menu.kind),
        Some(ActiveMenuKind::TerminalContext { session_id }) if *session_id == 7
    ));
    let entries = app.active_menu_entries();
    assert_eq!(entries.len(), 1);
    assert!(matches!(
        entries[0].action,
        MenuAction::OpenSftpFileManager {
            terminal_session_id
        } if terminal_session_id == 7
    ));
}

/// 验证 SFTP 文件行右键菜单展示下载、重命名和删除动作。
#[test]
fn sftp_entry_context_menu_shows_file_actions() {
    let mut app = test_app();
    let link_id = add_test_ssh_link(&mut app);
    let _receiver = insert_test_sftp_session(&mut app, 1, link_id);
    let remote_path = "/home/tester/app.log".to_string();
    if let Some(session) = app.sftp_sessions.get_mut(&1) {
        session.entries = vec![crate::remote::sftp::SftpEntry {
            name: "app.log".to_string(),
            path: remote_path.clone(),
            kind: crate::remote::sftp::SftpEntryKind::RegularFile,
            size: Some(128),
            mtime: None,
            permissions: Some(0o100644),
        }];
    }

    app.open_sftp_entry_context_menu(
        1,
        remote_path.clone(),
        gpui::point(gpui::px(2.0), gpui::px(3.0)),
    );

    assert!(matches!(
        app.active_menu.as_ref().map(|menu| &menu.kind),
        Some(ActiveMenuKind::SftpEntry { session_id }) if *session_id == 1
    ));
    assert_eq!(
        app.sftp_sessions
            .get(&1)
            .expect("应存在 SFTP 会话")
            .selected_paths
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec![remote_path]
    );
    let entries = app.active_menu_entries();
    assert_eq!(entries.len(), 4);
    assert!(matches!(
        entries[0].action,
        MenuAction::PreviewSftpSelection { session_id } if session_id == 1
    ));
    assert!(matches!(
        entries[1].action,
        MenuAction::DownloadSftpSelection { session_id } if session_id == 1
    ));
    assert!(matches!(
        entries[2].action,
        MenuAction::RenameSftpSelection { session_id } if session_id == 1
    ));
    assert!(matches!(
        entries[3].action,
        MenuAction::DeleteSftpSelection { session_id } if session_id == 1
    ));
    assert!(entries[3].is_danger);
}

/// 验证右键已选集合内文件时保留多选，方便从菜单下载多个文件。
#[test]
fn sftp_entry_context_menu_preserves_existing_multi_selection() {
    let mut app = test_app();
    let link_id = add_test_ssh_link(&mut app);
    let _receiver = insert_test_sftp_session(&mut app, 1, link_id);
    let first_path = "/home/tester/app.log".to_string();
    let second_path = "/home/tester/error.log".to_string();
    if let Some(session) = app.sftp_sessions.get_mut(&1) {
        session.entries = vec![
            crate::remote::sftp::SftpEntry {
                name: "app.log".to_string(),
                path: first_path.clone(),
                kind: crate::remote::sftp::SftpEntryKind::RegularFile,
                size: Some(128),
                mtime: None,
                permissions: Some(0o100644),
            },
            crate::remote::sftp::SftpEntry {
                name: "error.log".to_string(),
                path: second_path.clone(),
                kind: crate::remote::sftp::SftpEntryKind::RegularFile,
                size: Some(256),
                mtime: None,
                permissions: Some(0o100644),
            },
        ];
        session.selected_paths.insert(first_path.clone());
        session.selected_paths.insert(second_path.clone());
    }

    app.open_sftp_entry_context_menu(1, second_path, gpui::point(gpui::px(2.0), gpui::px(3.0)));

    let selected_paths = &app
        .sftp_sessions
        .get(&1)
        .expect("应存在 SFTP 会话")
        .selected_paths;
    assert_eq!(selected_paths.len(), 2);
    assert!(selected_paths.contains(&first_path));
    assert!(selected_paths.contains("/home/tester/error.log"));
}

/// 验证同一个 SSH 链接可以打开多个独立 SFTP 文件管理标签。
#[test]
fn sftp_file_manager_tabs_allow_multiple_same_link() {
    let mut app = test_app();
    let link_id = add_test_ssh_link(&mut app);
    let _first_receiver = insert_test_sftp_session(&mut app, 1, link_id);
    let _second_receiver = insert_test_sftp_session(&mut app, 2, link_id);

    app.create_sftp_tab_for_session(1);
    app.create_sftp_tab_for_session(2);

    assert_eq!(app.tabs.len(), 2);
    assert!(matches!(
        app.tabs[0].kind,
        TabKind::SftpFileManager { session_id } if session_id == 1
    ));
    assert!(matches!(
        app.tabs[1].kind,
        TabKind::SftpFileManager { session_id } if session_id == 2
    ));
    assert_eq!(app.active_tab_id, app.tabs[1].id);
}

/// 验证关闭 SFTP 文件管理标签会断开并清理对应会话。
#[test]
fn close_sftp_tab_disconnects_session() {
    let mut app = test_app();
    let link_id = add_test_ssh_link(&mut app);
    let receiver = insert_test_sftp_session(&mut app, 1, link_id);
    app.tabs[0].title = "文件管理 - 测试服务器".to_string();
    app.tabs[0].kind = TabKind::SftpFileManager { session_id: 1 };
    app.active_tab_id = app.tabs[0].id;

    app.close_tab(app.tabs[0].id);

    assert!(app.sftp_sessions.is_empty());
    assert!(matches!(
        receiver.try_recv(),
        Ok(crate::remote::sftp::SftpCommand::Disconnect)
    ));
    assert!(matches!(app.tabs[0].kind, TabKind::Empty));
}

/// 验证 SFTP 删除入口只允许普通文件和目录，避免误删符号链接等特殊条目。
#[test]
fn sftp_delete_selection_rejects_special_entries() {
    let mut app = test_app();
    let link_id = add_test_ssh_link(&mut app);
    let _receiver = insert_test_sftp_session(&mut app, 1, link_id);
    let remote_path = "/home/tester/current".to_string();
    if let Some(session) = app.sftp_sessions.get_mut(&1) {
        session.entries = vec![crate::remote::sftp::SftpEntry {
            name: "current".to_string(),
            path: remote_path.clone(),
            kind: crate::remote::sftp::SftpEntryKind::Symlink,
            size: None,
            mtime: None,
            permissions: None,
        }];
        session.selected_paths.insert(remote_path);
    }

    assert!(!app.can_delete_sftp_selection(1));
    app.request_delete_sftp_entry(1);

    assert!(app.sftp_dialog.is_none());
    assert!(
        app.placeholder_notice
            .contains("仅支持删除普通文件或空目录")
    );
}

/// 验证 SFTP 忙碌状态下不会继续启用文件操作按钮。
#[test]
fn sftp_file_actions_are_disabled_while_busy() {
    let mut app = test_app();
    let link_id = add_test_ssh_link(&mut app);
    let _receiver = insert_test_sftp_session(&mut app, 1, link_id);
    let remote_path = "/home/tester/app.log".to_string();
    if let Some(session) = app.sftp_sessions.get_mut(&1) {
        session.entries = vec![crate::remote::sftp::SftpEntry {
            name: "app.log".to_string(),
            path: remote_path.clone(),
            kind: crate::remote::sftp::SftpEntryKind::RegularFile,
            size: Some(128),
            mtime: None,
            permissions: Some(0o100644),
        }];
        session.selected_paths.insert(remote_path);
        session.status = crate::remote::sftp::SftpStatus::Transferring;
    }

    assert!(!app.can_download_sftp_selection(1));
    assert!(!app.can_rename_sftp_selection(1));
    assert!(!app.can_delete_sftp_selection(1));
}

/// 验证单文件探测未完成的压缩包已被选中时，也能立即打开 Jstack 分析右键菜单。
#[test]
fn source_tree_context_menu_shows_jstack_action_for_pending_archive_probe() {
    let mut app = test_app();
    let mut registry = SourceRegistry::new();
    let archive_id = registry.allocate_id();
    registry.insert_node(SourceTreeNode {
        id: archive_id,
        parent_id: None,
        depth: 0,
        label: "thread.zip".to_string(),
        kind: SourceKind::Archive(crate::loader::archive::ArchiveFormat::Zip),
        location: SourceLocation::LocalPath(PathBuf::from("thread.zip")),
        metadata: SourceMetadata {
            size: Some(1024),
            children_loaded: false,
            is_loading: true,
            message: None,
        },
        selected: false,
        expanded: false,
    });
    registry.rebuild_all_indices();
    app.source_registry = registry;
    app.selected_search_source_ids.insert(archive_id);

    app.open_source_tree_context_menu(archive_id, gpui::point(gpui::px(1.0), gpui::px(1.0)));

    assert!(matches!(
        app.active_menu.as_ref().map(|menu| &menu.kind),
        Some(ActiveMenuKind::SourceTree { source_id }) if *source_id == archive_id
    ));
    assert!(matches!(
        app.active_menu_entries()[0].action,
        MenuAction::OpenJstackAnalysis { source_id } if source_id == archive_id
    ));
}

/// 验证压缩包内目录也能显示 Jstack 与 Runtime 分析入口。
#[test]
fn source_tree_context_menu_shows_analysis_actions_for_archive_directory() {
    let (mut app, archive_dir_id, _, _) = app_with_loaded_archive_directory();

    app.open_source_tree_context_menu(archive_dir_id, gpui::point(gpui::px(1.0), gpui::px(1.0)));

    assert!(matches!(
        app.active_menu.as_ref().map(|menu| &menu.kind),
        Some(ActiveMenuKind::SourceTree { source_id }) if *source_id == archive_dir_id
    ));
    assert_eq!(app.active_menu_entries().len(), 2);
    assert!(matches!(
        app.active_menu_entries()[0].action,
        MenuAction::OpenJstackAnalysis { source_id } if source_id == archive_dir_id
    ));
    assert!(matches!(
        app.active_menu_entries()[1].action,
        MenuAction::OpenRuntimeAnalysis { source_id } if source_id == archive_dir_id
    ));
}

/// 验证右键未选中文件时会把分析输入切换为该文件。
#[test]
fn jstack_context_selection_switches_to_right_clicked_file() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let error_log_id = source_id_by_label(&app, "error.log");
    app.selected_search_source_ids.insert(app_log_id);

    let source_ids = app.jstack_source_ids_for_context(error_log_id);

    assert_eq!(source_ids, vec![error_log_id]);
    assert_eq!(
        app.selected_search_source_ids,
        BTreeSet::from([error_log_id])
    );
    assert_eq!(app.last_source_selection_anchor, Some(error_log_id));
}

/// 验证本地目录右键触发 Jstack 分析时会把目录作为独立目标交给后台递归展开。
#[test]
fn jstack_context_accepts_local_directory_target() {
    let mut app = app_with_placeholder_sources();
    let logs_dir_id = source_id_by_label(&app, "logs");

    let source_ids = app.jstack_source_ids_for_context(logs_dir_id);
    let targets = app.jstack_targets_from_source_ids(&source_ids);

    assert_eq!(source_ids, vec![logs_dir_id]);
    assert_eq!(targets.len(), 1);
    assert!(matches!(targets[0].location, SourceLocation::LocalPath(_)));
    assert_eq!(targets[0].label, "logs");
}

/// 验证 Jstack 右键压缩包内目录时，会按来源树顺序收集已加载的后代日志文件。
#[test]
fn jstack_context_archive_directory_collects_loaded_descendants() {
    let (mut app, archive_dir_id, first_log_id, second_log_id) =
        app_with_loaded_archive_directory();

    let source_ids = app.jstack_source_ids_for_context(archive_dir_id);
    let targets = app.jstack_targets_from_source_ids(&source_ids);

    assert_eq!(source_ids, vec![first_log_id, second_log_id]);
    assert_eq!(targets.len(), 2);
    assert!(
        targets
            .iter()
            .all(|target| matches!(target.location, SourceLocation::ArchiveEntry { .. }))
    );
}

/// 验证右键已在多选集合中时，会按来源树可见顺序保留多选输入。
#[test]
fn jstack_context_selection_keeps_multi_selection_in_tree_order() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let error_log_id = source_id_by_label(&app, "error.log");
    let nested_log_id = source_id_by_label(&app, "nested.log");
    app.selected_search_source_ids = BTreeSet::from([nested_log_id, error_log_id, app_log_id]);

    let source_ids = app.jstack_source_ids_for_context(error_log_id);

    assert_eq!(source_ids, vec![app_log_id, error_log_id, nested_log_id]);
}

/// 验证创建 Jstack 分析 tab 会复用空 tab 并写入加载状态。
#[test]
fn creating_jstack_analysis_tab_reuses_empty_tab() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let error_log_id = source_id_by_label(&app, "error.log");
    let targets = app.jstack_targets_from_source_ids(&[app_log_id, error_log_id]);

    let (analysis_id, generation) = app
        .create_jstack_analysis_tab_state(targets)
        .expect("应能创建 Jstack 分析 tab");

    assert_eq!(generation, 1);
    assert_eq!(app.tabs.len(), 1);
    assert!(matches!(
        app.active_tab_kind(),
        TabKind::JstackAnalysis { analysis_id: active_id } if active_id == analysis_id
    ));
    let state = app
        .jstack_analysis_state(analysis_id)
        .expect("应保存分析状态");
    assert_eq!(state.targets.len(), 2);
    assert_eq!(
        state.active_states,
        BTreeSet::from([JstackThreadState::Runnable])
    );
    assert!(state.is_thread_filter_enabled);
    assert!(matches!(
        state.task_state,
        JstackAnalysisTaskState::Loading { .. }
    ));
    assert_eq!(app.active_tab_title(), "Jstack分析(2)");
}

/// 验证 Jstack 配置过滤开关默认开启，并可在分析页内临时关闭。
#[test]
fn toggling_jstack_thread_filter_updates_analysis_state() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let targets = app.jstack_targets_from_source_ids(&[app_log_id]);
    let (analysis_id, _) = app
        .create_jstack_analysis_tab_state(targets)
        .expect("应能创建 Jstack 分析 tab");

    assert!(
        app.jstack_analysis_state(analysis_id)
            .expect("应保存分析状态")
            .is_thread_filter_enabled
    );

    app.toggle_jstack_thread_filter(analysis_id);

    assert!(
        !app.jstack_analysis_state(analysis_id)
            .expect("应保存分析状态")
            .is_thread_filter_enabled
    );
    assert_eq!(app.placeholder_notice, "已关闭 Jstack 配置过滤");
}

/// 验证 Runtime 右键已在多选集合中时，会按来源树可见顺序保留多选输入。
#[test]
fn runtime_context_selection_keeps_multi_selection_in_tree_order() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let error_log_id = source_id_by_label(&app, "error.log");
    let nested_log_id = source_id_by_label(&app, "nested.log");
    app.selected_search_source_ids = BTreeSet::from([nested_log_id, error_log_id, app_log_id]);

    let targets = app.runtime_targets_for_context(error_log_id);

    assert_eq!(targets.len(), 3);
    assert_eq!(targets[0].source_id, app_log_id);
    assert_eq!(targets[1].source_id, error_log_id);
    assert_eq!(targets[2].source_id, nested_log_id);
    assert!(
        targets
            .iter()
            .all(|target| target.kind == RuntimeAnalysisTargetKind::File)
    );
}

/// 验证 Runtime 右键本地目录会生成目录目标，由后台递归展开。
#[test]
fn runtime_context_accepts_local_directory_target() {
    let mut app = app_with_placeholder_sources();
    let logs_dir_id = source_id_by_label(&app, "logs");

    let targets = app.runtime_targets_for_context(logs_dir_id);

    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].source_id, logs_dir_id);
    assert_eq!(targets[0].kind, RuntimeAnalysisTargetKind::Directory);
}

/// 验证 Runtime 右键压缩包内目录时，会把已加载的后代日志条目作为文件目标解析。
#[test]
fn runtime_context_archive_directory_collects_loaded_descendant_files() {
    let (mut app, archive_dir_id, first_log_id, second_log_id) =
        app_with_loaded_archive_directory();

    let targets = app.runtime_targets_for_context(archive_dir_id);

    assert_eq!(targets.len(), 2);
    assert_eq!(targets[0].source_id, first_log_id);
    assert_eq!(targets[1].source_id, second_log_id);
    assert!(
        targets
            .iter()
            .all(|target| target.kind == RuntimeAnalysisTargetKind::File)
    );
}

/// 验证创建 Runtime 分析 tab 会复用空 tab 并写入加载状态。
#[test]
fn creating_runtime_analysis_tab_reuses_empty_tab() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let error_log_id = source_id_by_label(&app, "error.log");
    let targets = app.runtime_targets_from_source_ids(&[app_log_id, error_log_id]);

    let (analysis_id, generation) = app
        .create_runtime_analysis_tab_state(targets)
        .expect("应能创建 Runtime 分析 tab");

    assert_eq!(generation, 1);
    assert_eq!(app.tabs.len(), 1);
    assert!(matches!(
        app.active_tab_kind(),
        TabKind::RuntimeAnalysis { analysis_id: active_id } if active_id == analysis_id
    ));
    let state = app
        .runtime_analysis_state(analysis_id)
        .expect("应保存 Runtime 分析状态");
    assert_eq!(state.targets.len(), 2);
    assert_eq!(state.result_type, RuntimeAnalysisResultType::Statistics);
    assert_eq!(state.summary_sort_key, RuntimeSummarySortKey::RequestCount);
    assert_eq!(
        state.summary_sort_direction,
        RuntimeSortDirection::Descending
    );
    assert!(matches!(
        state.task_state,
        RuntimeAnalysisTaskState::Loading { .. }
    ));
    assert_eq!(app.active_tab_title(), "Runtime分析(2)");
}

/// 验证切换 Runtime 结果类型会清理旧表格交互态，统计下钻会回到统计分析。
#[test]
fn switching_runtime_result_type_clears_transient_state() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
    let (analysis_id, _) = app
        .create_runtime_analysis_tab_state(targets)
        .expect("应能创建 Runtime 分析 tab");
    {
        let state = app
            .runtime_analysis_state_mut(analysis_id)
            .expect("应存在 Runtime 分析状态");
        state.cell_selection = Some(RuntimeTableCellSelection {
            cell_key: "summary:0:path".to_string(),
            text: "/api/test".to_string(),
            anchor: 0,
            focus: 4,
        });
        state.cell_selection_drag = Some(RuntimeTableCellSelectionDrag {
            cell_key: "summary:0:path".to_string(),
            text: "/api/test".to_string(),
            anchor_range: 0..4,
            granularity: TextSelectionGranularity::Character,
        });
        state.hovered_sql_cell = Some(RuntimeSqlCellKey::Record {
            request_index: 0,
            sql_index: 0,
        });
        state.sql_text_dialog = Some(RuntimeSqlTextDialog {
            request_path: "/api/test".to_string(),
            request_time_label: "2026-06-25 14:25:03".to_string(),
            username: "tester".to_string(),
            sql_text: "select 1".to_string(),
            selection: None,
            selection_drag: None,
        });
    }

    app.set_runtime_result_type(analysis_id, RuntimeAnalysisResultType::SqlFrequency, None);

    let state = app
        .runtime_analysis_state(analysis_id)
        .expect("应存在 Runtime 分析状态");
    assert_eq!(state.result_type, RuntimeAnalysisResultType::SqlFrequency);
    assert!(state.cell_selection.is_none());
    assert!(state.cell_selection_drag.is_none());
    assert!(state.hovered_sql_cell.is_none());
    assert!(state.sql_text_dialog.is_none());

    app.open_runtime_request_details(analysis_id, "/api/test".to_string());

    let state = app
        .runtime_analysis_state(analysis_id)
        .expect("应存在 Runtime 分析状态");
    assert_eq!(state.result_type, RuntimeAnalysisResultType::Statistics);
    assert!(matches!(
        state.view,
        RuntimeAnalysisView::RequestDetails { .. }
    ));
}

/// 验证切回统计分析不会清空 SQL 频率和慢 SQL 的懒计算缓存。
#[test]
fn switching_runtime_statistics_preserves_sql_analysis_caches() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
    let (analysis_id, _) = app
        .create_runtime_analysis_tab_state(targets)
        .expect("应能创建 Runtime 分析 tab");
    let filter = RuntimeSqlAnalysisFilterSnapshot::default();
    {
        let state = app
            .runtime_analysis_state_mut(analysis_id)
            .expect("应存在 Runtime 分析状态");
        state.result_type = RuntimeAnalysisResultType::SqlFrequency;
        state
            .sql_frequency_rows_cache
            .borrow_mut()
            .replace(RuntimeSqlFrequencyRowsCache {
                filter: filter.clone(),
                rows: Arc::new(vec![RuntimeSqlFrequencyAnalysisRow {
                    normalized_sql: "select ?".to_string(),
                    total_execute_ms: 12,
                    execute_count: 1,
                }]),
            });
        state
            .slow_sql_rows_cache
            .borrow_mut()
            .replace(RuntimeSlowSqlRowsCache {
                filter,
                rows: Arc::new(vec![RuntimeSlowSqlSummaryRow {
                    normalized_sql: "select ?".to_string(),
                    total_execute_ms: 12,
                    execute_count: 1,
                }]),
            });
    }

    app.set_runtime_result_type(analysis_id, RuntimeAnalysisResultType::Statistics, None);
    app.set_runtime_result_type(analysis_id, RuntimeAnalysisResultType::SqlFrequency, None);

    let state = app
        .runtime_analysis_state(analysis_id)
        .expect("应存在 Runtime 分析状态");
    assert!(state.sql_frequency_rows_cache.borrow().is_some());
    assert!(state.slow_sql_rows_cache.borrow().is_some());
}

/// 验证 SQL 频率详情动作会进入详情页，并可返回频率列表。
#[test]
fn runtime_sql_frequency_detail_open_and_back_updates_state() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
    let (analysis_id, _) = app
        .create_runtime_analysis_tab_state(targets)
        .expect("应能创建 Runtime 分析 tab");

    app.open_runtime_sql_frequency_detail(
        analysis_id,
        "select * from users where id = ?".to_string(),
    );

    let state = app
        .runtime_analysis_state(analysis_id)
        .expect("应存在 Runtime 分析状态");
    assert_eq!(state.result_type, RuntimeAnalysisResultType::SqlFrequency);
    assert_eq!(
        state.sql_frequency_detail_sql.as_deref(),
        Some("select * from users where id = ?")
    );

    app.show_runtime_sql_frequency_summary(analysis_id);

    let state = app
        .runtime_analysis_state(analysis_id)
        .expect("应存在 Runtime 分析状态");
    assert_eq!(state.result_type, RuntimeAnalysisResultType::SqlFrequency);
    assert!(state.sql_frequency_detail_sql.is_none());
}

/// 验证 Runtime 时间选择器点选日期时保留原时分秒，并保持浮层打开以便继续调时间。
#[test]
fn runtime_time_picker_date_selection_preserves_time() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
    let (analysis_id, _) = app
        .create_runtime_analysis_tab_state(targets)
        .expect("应能创建 Runtime 分析 tab");
    app.runtime_analysis_state_mut(analysis_id)
        .expect("应存在 Runtime 分析状态")
        .filter_start_time_input
        .value = "2026-06-25 14:25:03".to_string();

    app.open_runtime_time_picker(analysis_id, RuntimeFilterInputKind::StartTime);
    app.set_runtime_filter_date(
        analysis_id,
        RuntimeFilterInputKind::StartTime,
        2026,
        7,
        2,
        None,
    );

    let state = app
        .runtime_analysis_state(analysis_id)
        .expect("应存在 Runtime 分析状态");
    assert_eq!(state.filter_start_time_input.value, "2026-07-02 14:25:03");
    assert_eq!(
        state.open_time_picker,
        Some(RuntimeFilterInputKind::StartTime)
    );
}

/// 验证 Runtime 时间选择器可以通过页面主体点击对应的状态方法关闭。
#[test]
fn closing_runtime_time_picker_clears_open_panel() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
    let (analysis_id, _) = app
        .create_runtime_analysis_tab_state(targets)
        .expect("应能创建 Runtime 分析 tab");

    app.open_runtime_time_picker(analysis_id, RuntimeFilterInputKind::EndTime);

    assert!(app.close_runtime_time_picker(analysis_id));
    assert_eq!(
        app.runtime_analysis_state(analysis_id)
            .expect("应存在 Runtime 分析状态")
            .open_time_picker,
        None
    );
}

/// 验证 Runtime SQL 完整文本弹窗可以正常打开和关闭。
#[test]
fn runtime_sql_text_dialog_opens_and_closes() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
    let (analysis_id, _) = app
        .create_runtime_analysis_tab_state(targets)
        .expect("应能创建 Runtime 分析 tab");

    app.open_runtime_sql_text_dialog(
        analysis_id,
        RuntimeSqlTextDialog {
            request_path: "/api/test".to_string(),
            request_time_label: "2026-06-25 14:25:03".to_string(),
            username: "tester".to_string(),
            sql_text: "select *\nfrom test_table".to_string(),
            selection: None,
            selection_drag: None,
        },
    );

    assert_eq!(
        app.runtime_analysis_state(analysis_id)
            .expect("应存在 Runtime 分析状态")
            .sql_text_dialog
            .as_ref()
            .map(|dialog| dialog.sql_text.as_str()),
        Some("select *\nfrom test_table")
    );
    assert!(app.close_runtime_sql_text_dialog(analysis_id));
    assert!(
        app.runtime_analysis_state(analysis_id)
            .expect("应存在 Runtime 分析状态")
            .sql_text_dialog
            .is_none()
    );
}

/// 验证 Runtime SQL 弹窗正文选区跨行提取时保留换行和缩进。
#[test]
fn runtime_sql_text_selection_extracts_multiline_text() {
    let lines = runtime_sql_text_lines("select *\n  from test_table\nwhere id = 1");
    let selection = RuntimeSqlTextSelection {
        anchor: RuntimeSqlTextPosition {
            line_index: 0,
            column: 7,
        },
        focus: RuntimeSqlTextPosition {
            line_index: 2,
            column: 5,
        },
    };

    let selected =
        selected_runtime_sql_text_from_lines(&lines, &selection).expect("应能提取跨行 SQL 选区");

    assert_eq!(selected, "*\n  from test_table\nwhere");
}

/// 验证点击 SQL 弹窗其他位置时只清理正文选区，不关闭弹窗。
#[test]
fn clearing_runtime_sql_text_selection_keeps_dialog_open() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
    let (analysis_id, _) = app
        .create_runtime_analysis_tab_state(targets)
        .expect("应能创建 Runtime 分析 tab");

    app.open_runtime_sql_text_dialog(
        analysis_id,
        RuntimeSqlTextDialog {
            request_path: "/api/test".to_string(),
            request_time_label: "2026-06-25 14:25:03".to_string(),
            username: "tester".to_string(),
            sql_text: "select *\nfrom test_table".to_string(),
            selection: None,
            selection_drag: None,
        },
    );
    app.begin_runtime_sql_text_selection(
        analysis_id,
        0,
        "select *".to_string(),
        0,
        TextSelectionGranularity::Character,
    );
    assert!(app.update_runtime_sql_text_selection(analysis_id, 0, "select *".to_string(), 6));
    assert!(app.finish_runtime_sql_text_selection(analysis_id));

    assert!(app.clear_runtime_sql_text_selection(analysis_id));
    let dialog = app
        .runtime_analysis_state(analysis_id)
        .expect("应存在 Runtime 分析状态")
        .sql_text_dialog
        .as_ref()
        .expect("清理选区不应关闭 SQL 弹窗");
    assert!(dialog.selection.is_none());
    assert!(dialog.selection_drag.is_none());
}

/// 验证 Runtime 表格单元格拖拽只保留用户选择的局部文本范围。
#[test]
fn runtime_cell_selection_keeps_character_range() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
    let (analysis_id, _) = app
        .create_runtime_analysis_tab_state(targets)
        .expect("应能创建 Runtime 分析 tab");

    app.begin_runtime_cell_selection(
        analysis_id,
        "summary:0:path".to_string(),
        "/api/runtime/example".to_string(),
        5,
        TextSelectionGranularity::Character,
    );
    assert!(app.update_runtime_cell_selection(analysis_id, "summary:0:path", 12));
    assert!(app.finish_runtime_cell_selection(analysis_id));

    let selection = app
        .runtime_analysis_state(analysis_id)
        .and_then(|state| state.cell_selection.as_ref())
        .expect("应存在 Runtime 单元格选区");
    let range = selection.normalized_range().expect("应存在非空选区");
    assert_eq!(slice_character_range(&selection.text, range), "runtime");
}

/// 验证 Runtime 表格单元格双击会选中整个单元格内容。
#[test]
fn runtime_cell_double_click_selects_whole_cell() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
    let (analysis_id, _) = app
        .create_runtime_analysis_tab_state(targets)
        .expect("应能创建 Runtime 分析 tab");

    app.begin_runtime_cell_selection(
        analysis_id,
        "request:1:username".to_string(),
        "youyj".to_string(),
        2,
        TextSelectionGranularity::Line,
    );
    assert!(app.finish_runtime_cell_selection(analysis_id));

    let selection = app
        .runtime_analysis_state(analysis_id)
        .and_then(|state| state.cell_selection.as_ref())
        .expect("应存在 Runtime 单元格选区");
    let range = selection.normalized_range().expect("应存在非空选区");
    assert_eq!(slice_character_range(&selection.text, range), "youyj");
}

/// 验证点击 Runtime 单元格以外区域时可以清理已有单元格选区。
#[test]
fn clearing_runtime_cell_selection_removes_active_selection() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
    let (analysis_id, _) = app
        .create_runtime_analysis_tab_state(targets)
        .expect("应能创建 Runtime 分析 tab");

    app.begin_runtime_cell_selection(
        analysis_id,
        "summary:0:path".to_string(),
        "/api/runtime/example".to_string(),
        0,
        TextSelectionGranularity::Line,
    );
    assert!(app.finish_runtime_cell_selection(analysis_id));

    assert!(app.clear_runtime_cell_selection());
    assert!(
        app.runtime_analysis_state(analysis_id)
            .and_then(|state| state.cell_selection.as_ref())
            .is_none()
    );
    assert!(!app.clear_runtime_cell_selection());
}

/// 验证关闭 Runtime 分析 tab 会清理对应分析状态。
#[test]
fn closing_runtime_analysis_tab_clears_analysis_state() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
    let (analysis_id, _) = app
        .create_runtime_analysis_tab_state(targets)
        .expect("应能创建 Runtime 分析 tab");
    let tab_id = app.active_tab_id;

    app.close_tab(tab_id);

    assert!(app.runtime_analysis_state(analysis_id).is_none());
    assert!(matches!(app.active_tab_kind(), TabKind::Empty));
}

/// 验证 Jstack 线程名复制入口只记录用户拖选的局部文本范围。
#[test]
fn jstack_thread_name_selection_keeps_character_range() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let targets = app.jstack_targets_from_source_ids(&[app_log_id]);
    let (analysis_id, _) = app
        .create_jstack_analysis_tab_state(targets)
        .expect("应能创建 Jstack 分析 tab");

    app.begin_jstack_thread_name_selection(
        analysis_id,
        "worker-1#123".to_string(),
        "worker-1".to_string(),
        0,
        TextSelectionGranularity::Character,
    );
    assert!(app.update_jstack_thread_name_selection(analysis_id, "worker-1#123", 4));
    assert!(app.finish_jstack_thread_name_selection(analysis_id));

    let selection = app
        .jstack_analysis_state(analysis_id)
        .and_then(|state| state.thread_name_selection.as_ref())
        .expect("应保留非空线程名选区");
    let range = selection.normalized_range().expect("应存在非空选区");
    assert_eq!(slice_character_range(&selection.thread_name, range), "work");
}

/// 验证 Jstack 状态筛选开关可以增删状态并重置滚动句柄。
#[test]
fn toggling_jstack_state_filter_updates_active_states() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let targets = app.jstack_targets_from_source_ids(&[app_log_id]);
    let (analysis_id, _) = app
        .create_jstack_analysis_tab_state(targets)
        .expect("应能创建 Jstack 分析 tab");

    app.toggle_jstack_state_filter(analysis_id, JstackThreadState::Blocked);

    let state = app
        .jstack_analysis_state(analysis_id)
        .expect("应保存分析状态");
    assert!(state.active_states.contains(&JstackThreadState::Runnable));
    assert!(state.active_states.contains(&JstackThreadState::Blocked));

    app.toggle_jstack_state_filter(analysis_id, JstackThreadState::Runnable);

    let state = app
        .jstack_analysis_state(analysis_id)
        .expect("应保存分析状态");
    assert!(!state.active_states.contains(&JstackThreadState::Runnable));
    assert!(state.active_states.contains(&JstackThreadState::Blocked));
}

/// 验证 Jstack 可见行按当前状态筛选后的命中数量排序，而不是按隐藏状态参与的总频率排序。
#[test]
fn visible_jstack_rows_sort_by_filtered_hit_count() {
    let first = crate::analysis::jstack::parse_jstack_snapshot(
        SourceId(1),
        "001.log",
        "/tmp/001.log",
        r#""mostly-hidden" #1
   java.lang.Thread.State: RUNNABLE
"alpha-runnable" #2
   java.lang.Thread.State: RUNNABLE
"always-runnable" #3
   java.lang.Thread.State: RUNNABLE
"#,
    );
    let second = crate::analysis::jstack::parse_jstack_snapshot(
        SourceId(2),
        "002.log",
        "/tmp/002.log",
        r#""mostly-hidden" #1
   java.lang.Thread.State: WAITING (parking)
"mostly-hidden" #1
   java.lang.Thread.State: WAITING (parking)
"mostly-hidden" #1
   java.lang.Thread.State: WAITING (parking)
"mostly-hidden" #1
   java.lang.Thread.State: WAITING (parking)
"alpha-runnable" #2
   java.lang.Thread.State: RUNNABLE
"always-runnable" #3
   java.lang.Thread.State: RUNNABLE
"#,
    );
    let result = crate::analysis::jstack::build_analysis_result(vec![first, second], Vec::new(), 2);
    let active_states = BTreeSet::from([JstackThreadState::Runnable]);

    let row_names = visible_jstack_row_indices(&result, &active_states, None)
        .into_iter()
        .map(|index| result.rows[index].thread_name.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        row_names,
        vec!["alpha-runnable", "always-runnable", "mostly-hidden"]
    );
}

/// 验证线程详情按可见快照收集代表堆栈，不把同一文件内重复出现展开成多条同源记录。
#[test]
fn jstack_detail_occurrences_keep_one_stack_per_visible_snapshot() {
    let first = crate::analysis::jstack::parse_jstack_snapshot(
        SourceId(1),
        "001.log",
        "/tmp/001.log",
        r#""same-thread" #7 tid=0x7
   java.lang.Thread.State: RUNNABLE
        at app.First.one(First.java:1)
"same-thread" #7 tid=0x7
   java.lang.Thread.State: RUNNABLE
        at app.First.two(First.java:2)
"#,
    );
    let second = crate::analysis::jstack::parse_jstack_snapshot(
        SourceId(2),
        "002.log",
        "/tmp/002.log",
        r#""same-thread" #7 tid=0x7
   java.lang.Thread.State: RUNNABLE
        at app.Second.one(Second.java:1)
"#,
    );
    let result = crate::analysis::jstack::build_analysis_result(vec![first, second], Vec::new(), 2);
    let row = result
        .rows
        .iter()
        .find(|row| row.thread_name == "same-thread")
        .expect("应存在同一线程行");
    let active_states = BTreeSet::from([JstackThreadState::Runnable]);

    let occurrences = jstack_detail_occurrences_for_visible_cells(row, &active_states, 0, 2);

    assert_eq!(occurrences.len(), 2);
    assert_eq!(occurrences[0].snapshot_label, "001.log");
    assert_eq!(occurrences[0].occurrence_index, 2);
    assert_eq!(occurrences[1].snapshot_label, "002.log");
    assert_eq!(occurrences[1].occurrence_index, 1);
}

/// 验证单文件探测未完成的压缩包不会打断来源树 Shift 范围多选。
#[test]
fn shift_range_selection_includes_pending_single_file_archive_probe() {
    let mut app = test_app();
    let mut registry = SourceRegistry::new();
    let root_id = registry.allocate_id();
    registry.insert_node(SourceTreeNode {
        id: root_id,
        parent_id: None,
        depth: 0,
        label: "logs".to_string(),
        kind: SourceKind::Directory,
        location: SourceLocation::LocalPath(PathBuf::from("logs")),
        metadata: SourceMetadata {
            size: None,
            children_loaded: true,
            is_loading: false,
            message: None,
        },
        selected: false,
        expanded: true,
    });
    let first_id = registry.allocate_id();
    registry.insert_node(SourceTreeNode {
        id: first_id,
        parent_id: Some(root_id),
        depth: 1,
        label: "001.log".to_string(),
        kind: SourceKind::LogFile,
        location: SourceLocation::LocalPath(PathBuf::from("logs/001.log")),
        metadata: SourceMetadata {
            size: Some(10),
            children_loaded: true,
            is_loading: false,
            message: None,
        },
        selected: false,
        expanded: false,
    });
    let pending_archive_id = registry.allocate_id();
    registry.insert_node(SourceTreeNode {
        id: pending_archive_id,
        parent_id: Some(root_id),
        depth: 1,
        label: "002.zip".to_string(),
        kind: SourceKind::Archive(crate::loader::archive::ArchiveFormat::Zip),
        location: SourceLocation::LocalPath(PathBuf::from("logs/002.zip")),
        metadata: SourceMetadata {
            size: Some(20),
            children_loaded: false,
            is_loading: false,
            message: None,
        },
        selected: false,
        expanded: false,
    });
    let last_id = registry.allocate_id();
    registry.insert_node(SourceTreeNode {
        id: last_id,
        parent_id: Some(root_id),
        depth: 1,
        label: "003.log".to_string(),
        kind: SourceKind::LogFile,
        location: SourceLocation::LocalPath(PathBuf::from("logs/003.log")),
        metadata: SourceMetadata {
            size: Some(30),
            children_loaded: true,
            is_loading: false,
            message: None,
        },
        selected: false,
        expanded: false,
    });
    registry.rebuild_all_indices();
    app.source_registry = registry;
    app.selected_search_source_ids.insert(first_id);
    app.last_source_selection_anchor = Some(first_id);

    app.select_source_tree_range_for_search(last_id);

    assert_eq!(
        app.selected_search_source_ids,
        BTreeSet::from([first_id, pending_archive_id, last_id])
    );
}

/// 验证来源树过滤态下，未完成单文件探测的压缩包仍参与 Shift 范围多选。
#[test]
fn source_tree_filter_keeps_pending_archive_for_shift_range_selection() {
    let mut app = test_app();
    let mut registry = SourceRegistry::new();
    let root_id = registry.allocate_id();
    registry.insert_node(SourceTreeNode {
        id: root_id,
        parent_id: None,
        depth: 0,
        label: "logs".to_string(),
        kind: SourceKind::Directory,
        location: SourceLocation::LocalPath(PathBuf::from("logs")),
        metadata: SourceMetadata {
            children_loaded: true,
            ..SourceMetadata::default()
        },
        selected: false,
        expanded: true,
    });

    let source_specs = [
        ("thread001.log", SourceKind::LogFile, true),
        (
            "thread002.zip",
            SourceKind::Archive(crate::loader::archive::ArchiveFormat::Zip),
            false,
        ),
        ("thread003.log", SourceKind::LogFile, true),
    ];
    let mut ids = Vec::new();
    for (label, kind, children_loaded) in source_specs {
        let source_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: source_id,
            parent_id: Some(root_id),
            depth: 1,
            label: label.to_string(),
            kind,
            location: SourceLocation::LocalPath(PathBuf::from(format!("logs/{label}"))),
            metadata: SourceMetadata {
                size: Some(10),
                children_loaded,
                is_loading: !children_loaded,
                message: None,
            },
            selected: false,
            expanded: false,
        });
        ids.push(source_id);
    }
    registry.rebuild_all_indices();
    app.source_registry = registry;

    app.open_source_tree_search();
    app.update_source_tree_search_query("thread".to_string());
    app.selected_search_source_ids.insert(ids[0]);
    app.last_source_selection_anchor = Some(ids[0]);

    app.select_source_tree_range_for_search(ids[2]);

    assert_eq!(app.selected_search_source_ids, BTreeSet::from_iter(ids));
}

/// 验证探测期间可见列表短暂缺少中间节点时，Shift 范围选择会用稳定树序补齐。
#[test]
fn shift_range_selection_fills_pending_archives_from_tree_order_during_probe() {
    let mut app = test_app();
    let mut registry = SourceRegistry::new();
    let root_id = registry.allocate_id();
    registry.insert_node(SourceTreeNode {
        id: root_id,
        parent_id: None,
        depth: 0,
        label: "logs".to_string(),
        kind: SourceKind::Directory,
        location: SourceLocation::LocalPath(PathBuf::from("logs")),
        metadata: SourceMetadata {
            children_loaded: true,
            ..SourceMetadata::default()
        },
        selected: false,
        expanded: true,
    });

    let mut source_ids = Vec::new();
    for index in 0..5 {
        let source_id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id: source_id,
            parent_id: Some(root_id),
            depth: 1,
            label: format!("thread{index:03}.zip"),
            kind: SourceKind::Archive(crate::loader::archive::ArchiveFormat::Zip),
            location: SourceLocation::LocalPath(PathBuf::from(format!(
                "logs/thread{index:03}.zip"
            ))),
            metadata: SourceMetadata {
                size: Some(1024),
                children_loaded: false,
                is_loading: true,
                message: None,
            },
            selected: false,
            expanded: false,
        });
        source_ids.push(source_id);
    }
    registry.rebuild_all_indices();
    app.source_registry = registry;
    app.is_source_tree_search_open = true;
    app.source_tree_search_input.value = "thread".to_string();
    app.filtered_source_ids = vec![root_id, source_ids[0], source_ids[4]];
    app.source_archive_probe_queue
        .extend(source_ids.iter().copied());
    app.source_archive_probe_queued_ids
        .extend(source_ids.iter().copied());
    assert!(app.select_pending_archive_probe_for_search_anchor(source_ids[0]));

    app.select_source_tree_range_for_search(source_ids[4]);

    assert_eq!(
        app.selected_search_source_ids,
        BTreeSet::from_iter(source_ids)
    );
}

/// 验证关闭 Jstack 分析 tab 会清理对应分析状态。
#[test]
fn closing_jstack_analysis_tab_clears_analysis_state() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let targets = app.jstack_targets_from_source_ids(&[app_log_id]);
    let (analysis_id, _) = app
        .create_jstack_analysis_tab_state(targets)
        .expect("应能创建 Jstack 分析 tab");
    let tab_id = app.active_tab_id;

    app.close_tab(tab_id);

    assert!(app.jstack_analysis_state(analysis_id).is_none());
    assert!(matches!(app.active_tab_kind(), TabKind::Empty));
}

/// 验证正式启动时来源树为空，左侧由空态图标承接而非展示样例数据。
#[test]
fn new_app_starts_with_empty_source_tree() {
    let app = test_app();

    assert!(app.source_registry.is_empty());
    assert!(app.visible_source_ids().is_empty());
}

/// 验证正式启动时内容区只保留空标签，不注入样例日志标签。
#[test]
fn new_app_starts_with_empty_log_tab() {
    let app = test_app();

    assert_eq!(app.active_tab_title(), "未选择日志");
    assert!(matches!(app.active_tab_kind(), TabKind::Empty));
}

/// 验证设置分类切换会更新右侧内容状态，并关闭上一分类的临时交互状态。
#[test]
fn selecting_settings_section_clears_transient_input_state() {
    let mut app = test_app();
    app.is_theme_dropdown_open = true;
    app.settings_quick_keywords_input.is_focused = true;

    app.select_settings_section(SettingsSection::LogSearch);

    assert_eq!(app.selected_settings_section, SettingsSection::LogSearch);
    assert!(!app.is_theme_dropdown_open);
    assert!(!app.settings_quick_keywords_input.is_focused);
}

/// 验证关闭设置模态框会同步清理模态状态和输入焦点。
#[test]
fn closing_settings_modal_clears_modal_state() {
    let mut app = test_app();
    app.is_settings_modal_open = true;
    app.settings_jstack_thread_name_filter_input.is_focused = true;

    app.close_settings_modal();

    assert!(!app.is_settings_modal_open);
    assert!(!app.settings_jstack_thread_name_filter_input.is_focused);
}

/// 验证同一日志来源重复点击时复用已有标签页。
#[test]
fn selecting_same_log_reuses_existing_tab() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");

    app.select_source(app_log_id);
    let tab_id = app.active_tab_id;
    app.select_source(app_log_id);

    assert_eq!(app.active_tab_id, tab_id);
    assert_eq!(app.tabs.len(), 1);
    assert!(matches!(
        app.active_tab_kind(),
        TabKind::LogSource {
            source_id,
            ..
        } if source_id == app_log_id
    ));
}

/// 验证不同日志来源会打开独立标签页。
#[test]
fn selecting_different_logs_opens_different_tabs() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let error_log_id = source_id_by_label(&app, "error.log");

    app.select_source(app_log_id);
    app.select_source(error_log_id);

    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active_tab_title(), "error.log");
    assert!(app.tabs.iter().any(|tab| tab.title == "app.log"));
    assert!(app.tabs.iter().any(|tab| tab.title == "error.log"));
}

/// 验证日志搜索快捷键只在日志正文拥有业务焦点时允许触发。
#[test]
fn log_search_shortcut_requires_log_text_focus() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");

    assert!(!app.is_active_log_view_focused());

    app.select_source(app_log_id);
    assert!(!app.is_active_log_view_focused());

    app.focus_log_text_view(app.active_tab_id);
    assert!(app.is_active_log_view_focused());

    app.close_tab(app.active_tab_id);
    assert!(!app.is_active_log_view_focused());
}

/// 验证日志行号打点在同一行重复点击时会添加再移除。
#[test]
fn toggling_log_line_marker_adds_and_removes_line() {
    let mut app = test_app();
    let tab_id = app.active_tab_id;

    app.toggle_log_line_marker(tab_id, 9);
    assert!(
        app.log_tab_view_state(tab_id)
            .is_some_and(|state| state.line_markers.contains(&9))
    );
    assert!(app.placeholder_notice.contains("已添加第 10 行"));

    app.toggle_log_line_marker(tab_id, 9);
    assert!(
        app.log_tab_view_state(tab_id)
            .is_some_and(|state| state.line_markers.is_empty())
    );
    assert!(app.placeholder_notice.contains("已移除第 10 行"));
}

/// 验证手动切换打点会清除上一轮 F2 跳转缓存，下一次跳转应从当前视口重新计算。
#[test]
fn toggling_log_line_marker_clears_last_jump_cache() {
    let mut app = test_app();
    let tab_id = app.active_tab_id;
    app.toggle_log_line_marker(tab_id, 9);
    app.log_tab_view_state_mut(tab_id)
        .expect("测试应用应存在默认日志视图状态")
        .last_line_marker_jump = Some(9);

    app.toggle_log_line_marker(tab_id, 12);

    assert!(
        app.log_tab_view_state(tab_id)
            .is_some_and(|state| state.last_line_marker_jump.is_none())
    );
}

/// 验证关闭日志标签页时会释放该标签的打点状态。
#[test]
fn closing_tab_clears_line_markers_for_that_tab() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");

    app.select_source(app_log_id);
    let tab_id = app.active_tab_id;
    app.toggle_log_line_marker(tab_id, 2);
    app.close_tab(tab_id);

    assert!(
        app.log_tab_view_state(tab_id)
            .is_some_and(|state| state.line_markers.is_empty())
    );
    assert!(matches!(app.active_tab_kind(), TabKind::Empty));
}

/// 验证激活日志标签页会展开来源树路径，并把非主动单选收束到当前文件。
#[test]
fn activating_log_tab_syncs_single_source_tree_selection() {
    let mut app = app_with_placeholder_sources();
    let logs_id = source_id_by_label(&app, "logs");
    let app_log_id = source_id_by_label(&app, "app.log");
    let error_log_id = source_id_by_label(&app, "error.log");

    app.select_source(app_log_id);
    let app_tab_id = app.active_tab_id;
    app.select_source(error_log_id);
    if app
        .source_registry
        .node(logs_id)
        .map(|source| source.expanded)
        .unwrap_or(false)
    {
        app.source_registry.toggle_expanded(logs_id);
    }

    assert!(!app.visible_source_ids().contains(&app_log_id));

    app.activate_tab(app_tab_id);

    assert_eq!(app.active_tab_id, app_tab_id);
    assert!(
        app.source_registry
            .node(app_log_id)
            .map(|source| source.selected)
            .unwrap_or(false)
    );
    assert!(
        !app.source_registry
            .node(error_log_id)
            .map(|source| source.selected)
            .unwrap_or(false)
    );
    assert!(
        app.source_registry
            .node(logs_id)
            .map(|source| source.expanded)
            .unwrap_or(false)
    );
    assert!(app.visible_source_ids().contains(&app_log_id));
    assert_eq!(app.selected_search_source_ids, BTreeSet::from([app_log_id]));
}

/// 验证激活日志标签页不会破坏用户主动多选的搜索文件范围。
#[test]
fn activating_log_tab_preserves_multi_source_search_selection() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let error_log_id = source_id_by_label(&app, "error.log");
    let nested_log_id = source_id_by_label(&app, "nested.log");

    app.select_source(app_log_id);
    let app_tab_id = app.active_tab_id;
    app.select_source(error_log_id);
    app.selected_search_source_ids = BTreeSet::from([error_log_id, nested_log_id]);

    app.activate_tab(app_tab_id);

    assert_eq!(app.active_tab_id, app_tab_id);
    assert!(
        app.source_registry
            .node(app_log_id)
            .map(|source| source.selected)
            .unwrap_or(false)
    );
    assert_eq!(
        app.selected_search_source_ids,
        BTreeSet::from([error_log_id, nested_log_id])
    );
}

/// 验证关闭当前标签后会激活相邻标签，关闭最后一个标签会回到空标签。
#[test]
fn close_tab_activates_neighbor_and_keeps_one_empty_tab() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let error_log_id = source_id_by_label(&app, "error.log");
    app.select_source(app_log_id);
    app.select_source(error_log_id);
    let error_tab_id = app.active_tab_id;

    app.close_tab(error_tab_id);
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.active_tab_title(), "app.log");

    let last_tab_id = app.active_tab_id;
    app.close_tab(last_tab_id);
    assert_eq!(app.tabs.len(), 1);
    assert!(matches!(app.active_tab_kind(), TabKind::Empty));
    assert_eq!(app.active_tab_title(), "未选择日志");
}

/// 验证标签右键菜单会记录目标标签和窗口锚点。
#[test]
fn tab_context_menu_records_target_tab_and_anchor() {
    let mut app = test_app();
    let target_tab_id = app.active_tab_id;
    let anchor = gpui::point(gpui::px(120.0), gpui::px(40.0));

    app.open_tab_context_menu(target_tab_id, anchor);

    let Some(active_menu) = app.active_menu.as_ref() else {
        panic!("右键标签后应打开活动菜单");
    };
    assert!(matches!(
        active_menu.kind,
        ActiveMenuKind::TabContext { tab_id } if tab_id == target_tab_id
    ));
    assert_eq!(active_menu.anchor, anchor);
}

/// 验证关闭其他标签只保留目标标签并激活它。
#[test]
fn close_other_tabs_keeps_target_tab_active() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let error_log_id = source_id_by_label(&app, "error.log");

    app.select_source(app_log_id);
    let app_tab_id = app.active_tab_id;
    app.select_source(error_log_id);
    app.close_other_tabs(app_tab_id);

    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.active_tab_id, app_tab_id);
    assert_eq!(app.active_tab_title(), "app.log");
}

/// 验证关闭全部标签后仍保留一个空标签承接界面。
#[test]
fn close_all_tabs_keeps_single_empty_tab() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");

    app.select_source(app_log_id);
    app.close_all_tabs();

    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.active_tab_title(), "未选择日志");
    assert!(matches!(app.active_tab_kind(), TabKind::Empty));
}

/// 验证标签溢出菜单项点击后会激活目标标签并关闭菜单。
#[test]
fn overflow_menu_action_activates_tab_and_closes_menu() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let error_log_id = source_id_by_label(&app, "error.log");

    app.select_source(app_log_id);
    let app_tab_id = app.active_tab_id;
    app.select_source(error_log_id);
    app.open_tab_overflow_menu(gpui::point(gpui::px(200.0), gpui::px(40.0)));
    app.handle_menu_action(MenuAction::ActivateTab { tab_id: app_tab_id });

    assert_eq!(app.active_tab_id, app_tab_id);
    assert_eq!(app.active_tab_title(), "app.log");
    assert!(app.active_menu.is_none());
}

/// 验证日志内容字号会被限制在外观设置允许范围内。
#[test]
fn adjust_log_content_font_size_clamps_to_range() {
    let mut app = test_app();

    app.adjust_log_content_font_size(100.0);
    assert_eq!(app.log_content_font_size, LOG_CONTENT_FONT_SIZE_MAX);

    app.adjust_log_content_font_size(-100.0);
    assert_eq!(app.log_content_font_size, LOG_CONTENT_FONT_SIZE_MIN);
}

/// 验证外观主题文件切换会立即替换运行时主题令牌。
#[test]
fn select_theme_updates_runtime_theme_tokens() {
    let mut app = test_app();

    app.select_theme("dark.toml".to_string());
    assert_eq!(app.selected_theme_id, "dark.toml");
    assert_eq!(
        app.theme.content,
        app.theme_manager.theme_for_id("dark.toml").content
    );
}

/// 验证旧版 light/system 配置会迁移到内置暗色主题文件。
#[test]
fn legacy_theme_modes_resolve_to_dark_theme_file() {
    let mut app = test_app();

    app.select_theme("light".to_string());

    assert_eq!(app.selected_theme_id, "dark.toml");
    assert_eq!(app.config.appearance.theme_mode, "dark.toml");
}

/// 验证外观和加载设置修改后会立即写入配置文件。
#[test]
fn settings_changes_are_persisted_to_config_file() {
    let config_manager = isolated_config_manager();
    let settings_path = config_manager.settings_path().to_path_buf();
    let mut app = ArgusApp::new_with_config_manager(config_manager);

    app.select_theme("dark.toml".to_string());
    app.adjust_log_content_font_size(2.0);
    app.adjust_max_archive_depth(1);
    app.adjust_archive_probe_concurrency(2);
    app.toggle_follow_symlinks();
    app.update_settings_quick_keywords("ERROR,WARN,timeout".to_string());
    app.update_settings_jstack_thread_name_filter("Attach Listener".to_string());
    app.update_settings_jstack_stack_segment_filter("Unsafe.park\n\nSocket\nread".to_string());

    let saved = ConfigManager::load_from_path(&settings_path).expect("设置变更后应写入配置文件");

    assert_eq!(saved.appearance.theme_mode, "dark.toml");
    assert_eq!(saved.appearance.log_content_font_size, 14.0);
    assert_eq!(saved.loader.max_archive_depth, 3);
    assert_eq!(saved.loader.archive_probe_concurrency, 6);
    assert!(saved.loader.follow_symlinks);
    assert_eq!(saved.log_search.quick_keywords, "ERROR,WARN,timeout");
    assert_eq!(
        saved.log_display.jstack_thread_name_filters,
        "Attach Listener"
    );
    assert_eq!(
        saved.log_display.jstack_stack_segment_filters,
        "Unsafe.park\n\nSocket\nread"
    );
}

/// 验证新日志来源加载成功后会替换旧来源，并清理旧日志相关界面状态。
#[test]
fn applying_new_load_report_replaces_old_log_workspace() {
    let mut app = app_with_placeholder_sources();
    app.has_loaded_real_sources = true;
    app.tabs.push(ArgusTab {
        id: 2,
        title: "old.log".to_string(),
        kind: TabKind::LogSource {
            source_id: SourceId(999),
            path: "old.log".to_string(),
        },
    });
    app.active_tab_id = 2;
    app.next_tab_id = 3;
    app.open_source_tree_search();
    app.update_source_tree_search_query("old".to_string());

    let mut registry = SourceRegistry::new();
    let new_id = registry.allocate_id();
    registry.insert_node(SourceTreeNode {
        id: new_id,
        parent_id: None,
        depth: 0,
        label: "new.log".to_string(),
        kind: SourceKind::LogFile,
        location: SourceLocation::LocalPath(PathBuf::from("new.log")),
        metadata: SourceMetadata {
            size: Some(128),
            children_loaded: true,
            is_loading: false,
            message: None,
        },
        selected: false,
        expanded: false,
    });
    registry.rebuild_all_indices();

    app.apply_load_report(LoadReport {
        registry,
        added_count: 1,
        skipped_count: 0,
        errors: Vec::new(),
        password_request: None,
    });

    assert_eq!(visible_labels(&app), vec!["new.log"]);
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.active_tab_title(), "未选择日志");
    assert!(matches!(app.active_tab_kind(), TabKind::Empty));
    assert_eq!(app.next_tab_id, 2);
    assert!(!app.is_source_tree_search_open);
    assert!(app.source_tree_search_input.value.is_empty());
    assert!(app.filtered_source_ids.is_empty());
}

/// 验证替换日志来源只清理日志页签，已打开的终端和远程文件管理页签继续保留。
#[test]
fn applying_new_load_report_keeps_connection_tabs_and_sessions() {
    let mut app = test_app();
    let link_id = add_test_ssh_link(&mut app);
    insert_test_terminal_session(&mut app, 7, link_id);
    let _sftp_receiver = insert_test_sftp_session(&mut app, 9, link_id);
    app.tabs = vec![
        ArgusTab {
            id: 4,
            title: "SSH 测试服务器".to_string(),
            kind: TabKind::SshTerminal { session_id: 7 },
        },
        ArgusTab {
            id: 5,
            title: "SFTP 测试服务器".to_string(),
            kind: TabKind::SftpFileManager { session_id: 9 },
        },
        ArgusTab {
            id: 8,
            title: "old.log".to_string(),
            kind: TabKind::LogSource {
                source_id: SourceId(999),
                path: "old.log".to_string(),
            },
        },
    ];
    app.active_tab_id = 5;
    app.next_tab_id = 10;

    app.apply_load_report(LoadReport {
        registry: placeholder_source_registry(),
        added_count: 1,
        skipped_count: 0,
        errors: Vec::new(),
        password_request: None,
    });

    assert_eq!(app.tabs.len(), 3);
    assert!(
        app.tabs.iter().any(|tab| {
            tab.id == 4 && matches!(tab.kind, TabKind::SshTerminal { session_id: 7 })
        })
    );
    assert!(app.tabs.iter().any(|tab| {
        tab.id == 5 && matches!(tab.kind, TabKind::SftpFileManager { session_id: 9 })
    }));
    assert!(!app.tabs.iter().any(|tab| tab.id == 8));
    assert!(matches!(app.active_tab_kind(), TabKind::Empty));
    assert_eq!(app.active_tab_id, 10);
    assert_eq!(app.next_tab_id, 11);
    assert!(app.terminal_sessions.contains_key(&7));
    assert!(app.sftp_sessions.contains_key(&9));

    let app_log_id = app
        .source_registry
        .tree_order_source_ids()
        .iter()
        .copied()
        .find(|source_id| {
            app.source_registry
                .node(*source_id)
                .is_some_and(|node| node.label == "app.log")
        })
        .expect("样例来源中应存在 app.log");
    app.open_or_focus_log_tab(app_log_id);

    assert_eq!(app.tabs.len(), 3);
    assert!(
        !app.tabs
            .iter()
            .any(|tab| matches!(tab.kind, TabKind::Empty))
    );
    assert!(matches!(
        app.active_tab_kind(),
        TabKind::LogSource { source_id, .. } if source_id == app_log_id
    ));
    assert_eq!(app.active_tab_id, 10);
    assert_eq!(app.next_tab_id, 11);
    assert!(app.terminal_sessions.contains_key(&7));
    assert!(app.sftp_sessions.contains_key(&9));
}

/// 验证来源树搜索只匹配日志候选节点，并保留其祖先目录上下文。
#[test]
fn source_tree_filter_matches_logs_and_keeps_ancestors() {
    let mut app = app_with_placeholder_sources();

    app.open_source_tree_search();
    app.update_source_tree_search_query("APP".to_string());

    assert_eq!(visible_labels(&app), vec!["logs", "app.log"]);
}

/// 验证来源树过滤不会改变真实目录树的展开状态。
#[test]
fn source_tree_filter_does_not_mutate_expansion_state() {
    let mut app = app_with_placeholder_sources();
    let logs_id = source_id_by_label(&app, "logs");

    app.source_registry.toggle_expanded(logs_id);
    app.open_source_tree_search();
    app.update_source_tree_search_query("app".to_string());

    assert!(!app.source_registry.node(logs_id).unwrap().expanded);
    assert_eq!(visible_labels(&app), vec!["logs", "app.log"]);
}

/// 验证切换到被当前过滤条件隐藏的日志标签时，只同步选中态，不修改过滤状态。
#[test]
fn activating_hidden_log_tab_keeps_source_tree_filter_and_updates_selection() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let error_log_id = source_id_by_label(&app, "error.log");

    app.select_source(app_log_id);
    let app_tab_id = app.active_tab_id;
    app.select_source(error_log_id);
    app.open_source_tree_search();
    app.update_source_tree_search_query("error".to_string());

    assert_eq!(visible_labels(&app), vec!["logs", "error.log"]);

    app.activate_tab(app_tab_id);

    assert_eq!(app.active_tab_id, app_tab_id);
    assert!(app.is_source_tree_search_open);
    assert_eq!(app.source_tree_search_input.value, "error");
    assert!(!app.visible_source_ids().contains(&app_log_id));
    assert!(
        app.source_registry
            .node(app_log_id)
            .map(|source| source.selected)
            .unwrap_or(false)
    );
    assert!(
        !app.source_registry
            .node(error_log_id)
            .map(|source| source.selected)
            .unwrap_or(false)
    );
}

/// 验证输入框编辑状态按字符索引移动，避免中文被按字节截断。
#[test]
fn source_tree_search_editing_uses_character_indices() {
    let mut app = test_app();

    app.insert_source_tree_search_text("日a志");
    app.move_source_tree_search_left(false);
    app.move_source_tree_search_left(true);

    assert_eq!(app.source_tree_search_input.cursor, 1);
    assert_eq!(app.source_tree_search_selection_range(), Some(1..2));

    app.insert_source_tree_search_text("b");
    assert_eq!(app.source_tree_search_input.value, "日b志");
    assert_eq!(app.source_tree_search_input.cursor, 2);
}

/// 验证全选和删除操作会同步维护光标和选区。
#[test]
fn source_tree_search_selection_delete_updates_cursor() {
    let mut app = test_app();

    app.update_source_tree_search_query("archive.log".to_string());
    app.select_all_source_tree_search();
    app.delete_source_tree_search_backward();

    assert!(app.source_tree_search_input.value.is_empty());
    assert_eq!(app.source_tree_search_input.cursor, 0);
    assert!(app.source_tree_search_selection_range().is_none());
}

/// 验证输入框鼠标拖拽按字符索引生成选区，中文不会被截断。
#[test]
fn source_tree_search_pointer_drag_selects_character_range() {
    let mut app = test_app();

    app.update_source_tree_search_query("日a志".to_string());
    app.begin_source_tree_search_pointer_selection(0, TextSelectionGranularity::Character);
    app.update_source_tree_search_pointer_selection(2);
    app.finish_source_tree_search_pointer_selection();

    assert_eq!(app.source_tree_search_selection_range(), Some(0..2));
    assert_eq!(
        app.selected_source_tree_search_text(),
        Some("日a".to_string())
    );
}

/// 验证输入框双击按词选择常见日志令牌，点号会作为分隔符。
#[test]
fn source_tree_search_double_click_selects_word() {
    let mut app = test_app();

    app.update_source_tree_search_query("中文 thread_001.zip java.lang.Class".to_string());
    app.begin_source_tree_search_pointer_selection(4, TextSelectionGranularity::Word);
    app.finish_source_tree_search_pointer_selection();

    assert_eq!(
        app.selected_source_tree_search_text(),
        Some("thread_001".to_string())
    );
}

/// 验证输入框三击会选中整个单行输入值。
#[test]
fn source_tree_search_triple_click_selects_whole_line() {
    let mut app = test_app();

    app.update_source_tree_search_query("archive.log".to_string());
    app.begin_source_tree_search_pointer_selection(3, TextSelectionGranularity::Line);
    app.finish_source_tree_search_pointer_selection();

    assert_eq!(
        app.selected_source_tree_search_text(),
        Some("archive.log".to_string())
    );
}

/// 验证日志双击选词支持中文、下划线令牌和点号分隔的 Java 类名片段。
#[test]
fn log_word_selection_supports_common_log_tokens() {
    let mut app = test_app();
    let tab_id = app.active_tab_id;
    let line = "中文 thread_001.zip java.lang.Class";

    app.select_log_word_at(tab_id, 0, line, 0);
    let selection = app
        .log_tab_view_state(tab_id)
        .and_then(|state| state.selection.as_ref())
        .unwrap();
    assert_eq!(selection.normalized().0.column, 0);
    assert_eq!(selection.normalized().1.column, 2);

    app.select_log_word_at(tab_id, 0, line, 4);
    let selection = app
        .log_tab_view_state(tab_id)
        .and_then(|state| state.selection.as_ref())
        .unwrap();
    assert_eq!(selection.normalized().0.column, 3);
    assert_eq!(selection.normalized().1.column, 13);

    app.select_log_word_at(tab_id, 0, line, 20);
    let selection = app
        .log_tab_view_state(tab_id)
        .and_then(|state| state.selection.as_ref())
        .unwrap();
    assert_eq!(selection.normalized().0.column, 18);
    assert_eq!(selection.normalized().1.column, 22);
}

/// 验证日志三击会选中整行展示文本，包含制表符展开后的列宽。
#[test]
fn log_triple_click_selects_whole_display_line() {
    let mut app = test_app();
    let tab_id = app.active_tab_id;

    app.select_log_text_line(tab_id, 7, "abc\tdef");
    let selection = app
        .log_tab_view_state(tab_id)
        .and_then(|state| state.selection.as_ref())
        .unwrap();
    let (start, end) = selection.normalized();

    assert_eq!(
        start,
        LogTextPosition {
            line_index: 7,
            column: 0
        }
    );
    assert_eq!(end.line_index, 7);
    assert_eq!(end.column, character_count("abc    def"));
}

/// 验证日志词级和行级拖拽会完整合并起始范围与当前范围。
#[test]
fn log_range_merge_expands_word_and_line_selection() {
    let word_anchor =
        log_text_range_for_granularity(0, "one two three", 1, TextSelectionGranularity::Word);
    let word_focus =
        log_text_range_for_granularity(0, "one two three", 5, TextSelectionGranularity::Word);
    let (word_start, word_end) = merge_log_text_ranges(&word_anchor, &word_focus).normalized();
    assert_eq!(
        word_start,
        LogTextPosition {
            line_index: 0,
            column: 0
        }
    );
    assert_eq!(
        word_end,
        LogTextPosition {
            line_index: 0,
            column: 7
        }
    );

    let line_anchor = log_text_range_for_granularity(1, "first", 2, TextSelectionGranularity::Line);
    let line_focus =
        log_text_range_for_granularity(3, "third line", 4, TextSelectionGranularity::Line);
    let (line_start, line_end) = merge_log_text_ranges(&line_anchor, &line_focus).normalized();
    assert_eq!(
        line_start,
        LogTextPosition {
            line_index: 1,
            column: 0
        }
    );
    assert_eq!(
        line_end,
        LogTextPosition {
            line_index: 3,
            column: 10
        }
    );
}

/// 构造只有一个未加载目录的应用状态，用于验证懒加载状态机。
fn app_with_loading_directory() -> (ArgusApp, SourceId) {
    let mut app = test_app();
    let mut registry = SourceRegistry::new();
    let id = registry.allocate_id();
    registry.insert_node(SourceTreeNode {
        id,
        parent_id: None,
        depth: 0,
        label: "logs".to_string(),
        kind: SourceKind::Directory,
        location: SourceLocation::LocalPath(PathBuf::from("logs")),
        metadata: SourceMetadata {
            size: None,
            children_loaded: false,
            is_loading: true,
            message: None,
        },
        selected: false,
        expanded: true,
    });
    registry.rebuild_all_indices();
    app.source_registry = registry;
    app.source_child_load_generations.insert(id, 1);
    (app, id)
}

/// 验证子级加载失败后不会标记为已加载，用户后续点击仍可重试。
#[test]
fn child_load_failure_keeps_node_retryable() {
    let (mut app, source_id) = app_with_loading_directory();
    let report = LoadReport {
        registry: SourceRegistry::new(),
        added_count: 0,
        skipped_count: 1,
        errors: vec!["权限不足".to_string()],
        password_request: None,
    };

    app.apply_child_load_report(source_id, 1, report);

    let node = app.source_registry.node(source_id).unwrap();
    assert!(!node.metadata.children_loaded);
    assert!(!node.metadata.is_loading);
    assert!(!node.expanded);
    assert_eq!(node.metadata.message.as_deref(), Some("权限不足"));
    assert!(!app.source_child_load_generations.contains_key(&source_id));
}

/// 验证过期的后台懒加载结果不会覆盖当前节点状态。
#[test]
fn stale_child_load_report_is_ignored() {
    let (mut app, source_id) = app_with_loading_directory();
    app.source_child_load_generations.insert(source_id, 2);
    let report = LoadReport {
        registry: SourceRegistry::new(),
        added_count: 0,
        skipped_count: 1,
        errors: vec!["旧结果".to_string()],
        password_request: None,
    };

    app.apply_child_load_report(source_id, 1, report);

    let node = app.source_registry.node(source_id).unwrap();
    assert!(node.metadata.is_loading);
    assert!(node.expanded);
    assert_eq!(
        app.source_child_load_generations.get(&source_id).copied(),
        Some(2)
    );
}
