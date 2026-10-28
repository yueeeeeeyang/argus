//! 文件职责：app 模块的单元测试。
//! 创建日期：2026-07-08
//! 修改日期：2026-10-28
//! 作者：Argus 开发团队
//! 主要功能：测试应用状态、来源树、标签页、搜索、分析和连接等核心行为。

use super::*;
use std::path::Path;
use std::path::PathBuf;

use crate::analysis::runtime::{build_runtime_analysis_result, parse_runtime_request_text};
use crate::config::JstackThreadFilterRuleKind;
use crate::config::paths::{isolated_test_dir, temporary_test_dir};
use crate::remote::connection::ConnectionLinkKind;

/// 构造隔离真实用户目录的配置管理器。
fn isolated_config_manager() -> ConfigManager {
    let config_dir = isolated_test_dir("app");
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
    let _receiver = insert_test_terminal_session_with_commands(app, session_id, link_id);
}

/// 插入不连接真实服务器的终端会话，并返回命令接收端。
fn insert_test_terminal_session_with_commands(
    app: &mut ArgusApp,
    session_id: usize,
    link_id: ConnectionNodeId,
) -> std::sync::mpsc::Receiver<crate::remote::terminal::TerminalCommand> {
    let link = app
        .config
        .connections
        .link(link_id)
        .expect("应存在测试链接")
        .clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut session =
        crate::remote::terminal::TerminalSessionState::connecting(session_id, &link, sender);
    session.status = crate::remote::terminal::TerminalStatus::Connected;
    app.terminal_sessions.insert(session_id, session);
    receiver
}

/// 取出终端命令接收端内的全部输入字节，忽略其它命令。
fn drain_terminal_input(
    receiver: &std::sync::mpsc::Receiver<crate::remote::terminal::TerminalCommand>,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    while let Ok(command) = receiver.try_recv() {
        if let crate::remote::terminal::TerminalCommand::Input(input) = command {
            bytes.extend(input);
        }
    }
    bytes
}

/// 构造终端按键测试输入。
fn terminal_keystroke(key: &str, key_char: Option<&str>, modifiers: gpui::Modifiers) -> Keystroke {
    Keystroke {
        key: key.to_string(),
        modifiers,
        key_char: key_char.map(ToString::to_string),
    }
}

/// 插入不连接真实服务器的 SFTP 会话，并返回命令接收端。
fn insert_test_sftp_session(
    app: &mut ArgusApp,
    session_id: usize,
    link_id: ConnectionNodeId,
) -> std::sync::mpsc::Receiver<crate::remote::remote_file::RemoteFileCommand> {
    let link = app
        .config
        .connections
        .link(link_id)
        .expect("应存在测试链接")
        .clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut session = crate::remote::remote_file::RemoteFileSessionState::connecting(
        session_id,
        &link,
        crate::remote::remote_file::RemoteFileBackend::Sftp,
        sender,
    );
    session.status = crate::remote::remote_file::RemoteFileStatus::Connected;
    session.current_dir = "/home/tester".to_string();
    session.address_input = TextInputState::from_value(session.current_dir.clone());
    app.remote_file_sessions.insert(session_id, session);
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

/// 在测试应用中直接创建一个日志标签并激活，返回标签 ID。
///
/// 说明：正式启动和关闭全部标签后都不再保留占位标签，依赖“已存在标签”的单元
/// 测试需要用它显式创建标签。
fn push_test_log_tab(app: &mut ArgusApp, title: &str) -> usize {
    let tab_id = app.next_tab_id;
    app.next_tab_id += 1;
    app.tabs.push(ArgusTab {
        id: tab_id,
        title: title.to_string(),
        kind: TabKind::LogSource {
            source_id: SourceId(tab_id),
            path: format!("/tmp/{title}"),
        },
    });
    app.active_tab_id = Some(tab_id);
    app.ensure_log_tab_view_state(tab_id);
    tab_id
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

/// 构造一个已加载的目录，模拟用户直接右键目录。
fn app_with_loaded_archive_directory() -> (ArgusApp, SourceId, SourceId, SourceId) {
    let mut app = test_app();
    let mut registry = SourceRegistry::new();
    let dir_id = registry.allocate_id();
    registry.insert_node(SourceTreeNode {
        id: dir_id,
        parent_id: None,
        depth: 0,
        label: "runtime".to_string(),
        kind: SourceKind::Directory,
        location: SourceLocation::LocalPath(PathBuf::from("runtime")),
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
        kind: SourceKind::LogFile,
        location: SourceLocation::LocalPath(PathBuf::from("runtime/thread0100.log")),
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
        kind: SourceKind::LogFile,
        location: SourceLocation::LocalPath(PathBuf::from("runtime/thread0200.log")),
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
    assert_eq!(app.active_menu_entries().len(), 3);
    assert!(matches!(
        app.active_menu_entries()[0].action,
        MenuAction::OpenJstackAnalysis { source_id } if source_id == app_log_id
    ));
    assert!(matches!(
        app.active_menu_entries()[1].action,
        MenuAction::OpenRuntimeAnalysis { source_id } if source_id == app_log_id
    ));
    assert!(matches!(
        app.active_menu_entries()[2].action,
        MenuAction::AddSourceToAssistant {
            source_id,
            session_id: None
        } if source_id == app_log_id
    ));

    app.close_active_menu();
    app.open_source_tree_context_menu(logs_dir_id, gpui::point(gpui::px(1.0), gpui::px(1.0)));

    assert!(matches!(
        app.active_menu.as_ref().map(|menu| &menu.kind),
        Some(ActiveMenuKind::SourceTree { source_id }) if *source_id == logs_dir_id
    ));
    assert_eq!(app.active_menu_entries().len(), 3);
    assert!(matches!(
        app.active_menu_entries()[0].action,
        MenuAction::OpenJstackAnalysis { source_id } if source_id == logs_dir_id
    ));
    assert!(matches!(
        app.active_menu_entries()[1].action,
        MenuAction::OpenRuntimeAnalysis { source_id } if source_id == logs_dir_id
    ));
    assert!(matches!(
        app.active_menu_entries()[2].action,
        MenuAction::AddSourceToAssistant {
            source_id,
            session_id: None
        } if source_id == logs_dir_id
    ));
}

/// 构造只含一个真实本地文件节点的来源树，供另存为相关用例复用。
///
/// 另有断言依赖文件真实存在，因此这里必须写入磁盘而不是使用占位路径。
fn app_with_real_source_file(scope: &str) -> (ArgusApp, PathBuf, SourceId) {
    let path = crate::config::paths::isolated_test_file_path(scope, "app.log");
    std::fs::write(&path, b"line one\nline two\n").expect("应能写入测试来源文件");

    let mut app = test_app();
    let mut registry = SourceRegistry::new();
    let file_id = registry.allocate_id();
    registry.insert_node(SourceTreeNode {
        id: file_id,
        parent_id: None,
        depth: 0,
        label: "app.log".to_string(),
        kind: SourceKind::LogFile,
        location: SourceLocation::LocalPath(path.clone()),
        metadata: SourceMetadata {
            size: Some(18),
            children_loaded: true,
            is_loading: false,
            message: None,
        },
        selected: false,
        expanded: false,
    });
    registry.rebuild_all_indices();
    app.source_registry = registry;
    (app, path, file_id)
}

/// 验证单个文件的右键菜单末项是另存为，并带另存为图标。
///
/// 说明：物化后的普通文件同时是分析候选，分析入口排在前面；另存为放最后，
/// 避免文件操作打断分析类入口。
#[test]
fn source_tree_context_menu_offers_save_as_for_single_file() {
    let (mut app, _path, file_id) = app_with_real_source_file("menu-save-as-file");

    app.open_source_tree_context_menu(file_id, gpui::point(gpui::px(1.0), gpui::px(1.0)));

    let entries = app.active_menu_entries();
    assert!(
        matches!(
            app.active_menu.as_ref().map(|menu| &menu.kind),
            Some(ActiveMenuKind::SourceTree { source_id }) if *source_id == file_id
        ),
        "单个文件必须能打开来源树菜单"
    );
    let save_as = entries.last().expect("菜单不应为空");
    assert_eq!(save_as.label, "另存为", "另存为应是最后一个菜单项");
    assert_eq!(
        save_as.icon,
        Some(crate::ui::components::icon::ArgusIcon::FileDown),
        "另存为应带文件下载图标"
    );
    assert!(matches!(
        save_as.action,
        MenuAction::SaveSourceAs { source_id } if source_id == file_id
    ));
    assert!(
        entries.iter().all(|entry| entry.icon.is_some()),
        "来源树菜单每一行都应带图标"
    );
}

/// 验证目录节点不提供另存为入口。
#[test]
fn source_tree_context_menu_omits_save_as_for_directory() {
    let mut app = app_with_placeholder_sources();
    let logs_dir_id = source_id_by_label(&app, "logs");

    assert!(
        !app.source_is_exportable_file(logs_dir_id),
        "目录不具备单文件另存语义"
    );

    app.open_source_tree_context_menu(logs_dir_id, gpui::point(gpui::px(1.0), gpui::px(1.0)));
    assert!(
        app.active_menu_entries()
            .iter()
            .all(|entry| !matches!(entry.action, MenuAction::SaveSourceAs { .. })),
        "目录菜单不应出现另存为"
    );
}

/// 验证只有真实存在的普通文件才被判定为可另存。
#[test]
fn source_is_exportable_file_requires_existing_regular_file() {
    let app = app_with_placeholder_sources();
    let missing_id = source_id_by_label(&app, "app.log");
    assert!(
        !app.source_is_exportable_file(missing_id),
        "占位路径指向的文件不存在时不得提供另存为入口"
    );

    let (real_app, _path, real_file_id) = app_with_real_source_file("menu-save-as-exists");
    assert!(
        real_app.source_is_exportable_file(real_file_id),
        "真实存在的普通文件必须可另存"
    );
}

/// 验证另存为动作会把文件写入目标目录，并在完成后如实报告结果。
#[gpui::test]
fn save_source_as_writes_file_and_reports_result(cx: &mut gpui::TestAppContext) {
    let source_dir = isolated_test_dir("save-as-source");
    let target_dir = isolated_test_dir("save-as-target");
    std::fs::create_dir_all(&source_dir).expect("应能创建来源目录");
    std::fs::create_dir_all(&target_dir).expect("应能创建目标目录");
    let source_path = source_dir.join("app.log");
    std::fs::write(&source_path, b"payload\n").expect("应能写入来源文件");

    let app = cx.new(|_| test_app());
    app.update(cx, |app, app_cx| {
        app.start_source_export(
            source_path.clone(),
            target_dir.clone(),
            "app.log".to_string(),
            app_cx,
        );
        assert!(
            app.placeholder_notice.contains("正在另存"),
            "导出开始时应提示正在进行：{}",
            app.placeholder_notice
        );
    });

    cx.run_until_parked();

    app.update(cx, |app, _app_cx| {
        assert_eq!(
            std::fs::read(target_dir.join("app.log")).expect("目标文件应已写出"),
            b"payload\n"
        );
        assert!(
            app.placeholder_notice.contains("已另存为"),
            "导出完成后应报告实际写入位置：{}",
            app.placeholder_notice
        );
        assert!(
            app.placeholder_notice.contains("app.log"),
            "提示应包含目标文件名：{}",
            app.placeholder_notice
        );
    });
}

/// 验证另存为失败时会如实报告错误，而不是静默失败。
#[gpui::test]
fn save_source_as_reports_failure_for_missing_target_directory(cx: &mut gpui::TestAppContext) {
    let source_dir = isolated_test_dir("save-as-missing-target");
    std::fs::create_dir_all(&source_dir).expect("应能创建来源目录");
    let source_path = source_dir.join("app.log");
    std::fs::write(&source_path, b"payload").expect("应能写入来源文件");

    let app = cx.new(|_| test_app());
    app.update(cx, |app, app_cx| {
        app.start_source_export(
            source_path.clone(),
            source_dir.join("does-not-exist"),
            "app.log".to_string(),
            app_cx,
        );
    });

    cx.run_until_parked();

    app.update(cx, |app, _app_cx| {
        assert!(
            app.placeholder_notice.contains("失败"),
            "目标目录不存在时必须报告失败：{}",
            app.placeholder_notice
        );
    });
}

/// 验证 SSH 终端正文右键菜单展示粘贴和文件管理入口。
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
    assert_eq!(entries.len(), 2);
    assert!(matches!(
        entries[0].action,
        MenuAction::PasteTerminalClipboard { session_id } if session_id == 7
    ));
    assert!(matches!(
        entries[1].action,
        MenuAction::OpenSftpFileManager {
            terminal_session_id
        } if terminal_session_id == 7
    ));
}

/// 验证终端右键菜单在有选区时展示复制入口，无选区时隐藏。
#[test]
fn terminal_context_menu_shows_copy_only_with_selection() {
    let mut app = test_app();
    let link_id = add_test_ssh_link(&mut app);
    insert_test_terminal_session(&mut app, 7, link_id);

    app.open_terminal_context_menu(7, gpui::point(gpui::px(1.0), gpui::px(1.0)));
    let actions = app
        .active_menu_entries()
        .into_iter()
        .map(|entry| entry.action)
        .collect::<Vec<_>>();
    assert!(!actions.contains(&MenuAction::CopyTerminalSelection { session_id: 7 }));
    assert!(actions.contains(&MenuAction::PasteTerminalClipboard { session_id: 7 }));

    {
        let session = app
            .terminal_sessions
            .get_mut(&7)
            .expect("应存在测试终端会话");
        session.process_output(b"hello world");
        session.begin_selection(crate::remote::terminal::TerminalGridPosition { row: 0, col: 0 });
        session.update_selection(crate::remote::terminal::TerminalGridPosition { row: 0, col: 5 });
    }
    app.open_terminal_context_menu(7, gpui::point(gpui::px(1.0), gpui::px(1.0)));
    let actions = app
        .active_menu_entries()
        .into_iter()
        .map(|entry| entry.action)
        .collect::<Vec<_>>();
    assert!(actions.contains(&MenuAction::CopyTerminalSelection { session_id: 7 }));
}

/// 验证终端粘贴会把剪贴板文本按终端回车语义发送给远程 shell。
#[gpui::test]
fn terminal_paste_sends_clipboard_with_normalized_newlines(cx: &mut gpui::TestAppContext) {
    let app = cx.new(|_| test_app());
    app.update(cx, |app, app_cx| {
        let link_id = add_test_ssh_link(app);
        let link = app
            .config
            .connections
            .link(link_id)
            .expect("应存在测试链接")
            .clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut session =
            crate::remote::terminal::TerminalSessionState::connecting(7, &link, sender);
        session.status = crate::remote::terminal::TerminalStatus::Connected;
        app.terminal_sessions.insert(7, session);

        app_cx.write_to_clipboard(gpui::ClipboardItem::new_string("ls\r\npwd\n".to_string()));
        app.paste_terminal_clipboard(7, app_cx);

        let Ok(crate::remote::terminal::TerminalCommand::Input(bytes)) = receiver.try_recv() else {
            panic!("粘贴应向远程 shell 发送输入字节");
        };
        assert_eq!(bytes, b"ls\rpwd\r");
    });
}

/// 验证新增链接菜单同时展示 SSH、SMB、Git 和 SVN 四种协议入口。
#[test]
fn connection_link_create_menu_shows_four_protocols() {
    let mut app = test_app();
    app.open_connection_link_create_menu(gpui::point(gpui::px(1.0), gpui::px(1.0)));

    let entries = app.active_menu_entries();
    assert_eq!(entries.len(), 4);
    assert!(entries.iter().all(|entry| entry.icon.is_some()));
    let actions = entries
        .into_iter()
        .map(|entry| entry.action)
        .collect::<Vec<_>>();
    assert!(actions.contains(&MenuAction::NewSshConnectionLink));
    assert!(actions.contains(&MenuAction::NewSmbConnectionLink));
    assert!(actions.contains(&MenuAction::NewGitConnectionLink));
    assert!(actions.contains(&MenuAction::NewSvnConnectionLink));
}

/// 验证目录右键菜单提供四种协议的新建入口以及编辑、删除目录动作。
#[test]
fn connection_tree_directory_menu_shows_create_entries() {
    let mut app = test_app();
    let directory_id = app
        .config
        .connections
        .add_directory(None, "生产环境")
        .unwrap();

    app.open_connection_tree_context_menu(directory_id, gpui::point(gpui::px(1.0), gpui::px(1.0)));

    let actions = app
        .active_menu_entries()
        .into_iter()
        .map(|entry| entry.action)
        .collect::<Vec<_>>();
    assert_eq!(app.selected_connection_node_id, Some(directory_id));
    assert!(
        app.active_menu_entries()
            .iter()
            .all(|entry| entry.icon.is_some())
    );
    assert!(actions.contains(&MenuAction::NewSshConnectionLink));
    assert!(actions.contains(&MenuAction::NewSmbConnectionLink));
    assert!(actions.contains(&MenuAction::NewGitConnectionLink));
    assert!(actions.contains(&MenuAction::NewSvnConnectionLink));
    assert!(actions.contains(&MenuAction::EditConnectionNode {
        node_id: directory_id
    }));
    assert!(actions.contains(&MenuAction::DeleteConnectionNode {
        node_id: directory_id
    }));
}

/// 验证 SSH 链接右键菜单提供文件管理入口，其他协议链接不展示。
#[test]
fn connection_tree_link_menu_shows_file_manager_only_for_ssh() {
    let mut app = test_app();
    let ssh_link_id = add_test_ssh_link(&mut app);

    app.open_connection_tree_context_menu(ssh_link_id, gpui::point(gpui::px(1.0), gpui::px(1.0)));
    let actions = app
        .active_menu_entries()
        .into_iter()
        .map(|entry| entry.action)
        .collect::<Vec<_>>();
    assert!(actions.contains(&MenuAction::OpenSftpFileManagerFromLink {
        link_id: ssh_link_id
    }));

    let smb_link_id = app
        .config
        .connections
        .add_smb_link(
            None,
            "共享日志",
            crate::remote::connection::SmbLinkConfig {
                host: "10.0.0.2".to_string(),
                port: 445,
                share: "logs".to_string(),
                initial_dir: "/".to_string(),
                domain: None,
                username: "smbuser".to_string(),
                password: "secret".to_string(),
            },
        )
        .unwrap();
    app.open_connection_tree_context_menu(smb_link_id, gpui::point(gpui::px(1.0), gpui::px(1.0)));
    let actions = app
        .active_menu_entries()
        .into_iter()
        .map(|entry| entry.action)
        .collect::<Vec<_>>();
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, MenuAction::OpenSftpFileManagerFromLink { .. })),
        "SMB 链接菜单不应展示 SFTP 文件管理入口"
    );
}

/// 验证目录右键新建链接会以右键选中的目录作为父目录打开新增对话框。
#[gpui::test]
fn connection_tree_directory_menu_create_dialog_uses_right_clicked_parent(
    cx: &mut gpui::TestAppContext,
) {
    let app = cx.new(|_| test_app());
    app.update(cx, |app, app_cx| {
        let directory_id = app
            .config
            .connections
            .add_directory(None, "生产环境")
            .unwrap();
        app.open_connection_tree_context_menu(
            directory_id,
            gpui::point(gpui::px(1.0), gpui::px(1.0)),
        );

        app.handle_menu_action_with_context(MenuAction::NewSshConnectionLink, None, app_cx);

        assert!(app.active_menu.is_none());
        let modal = app
            .connection_link_modal
            .as_ref()
            .expect("应打开新增链接模态框");
        modal.read_with(app_cx, |modal_state, _| {
            assert_eq!(modal_state.link_kind(), ConnectionLinkKind::Ssh);
            assert_eq!(modal_state.form_parent_id(), Some(directory_id));
        });
    });
}

/// 验证表单弹窗的操作按钮与下边框间距等于标题到上边框的间距，避免按钮贴边或底部大片空白。
#[gpui::test]
fn form_dialogs_keep_symmetric_vertical_padding(cx: &mut gpui::TestAppContext) {
    use crate::app::{ArchivePasswordPromptState, ArchivePasswordRetryAction};
    use crate::loader::archive::ArchivePasswordError;
    use crate::ui::components::modal_dialog::MODAL_BOTTOM_PADDING;

    let (app, cx) = cx.add_window_view(|_, _| app_with_placeholder_sources());
    cx.simulate_resize(gpui::size(gpui::px(1400.0), gpui::px(900.0)));

    // 连接弹窗根节点自带 1px 边框，内容整体向内缩 1px，从外壳边界量到按钮应为 19 + 1。
    let bordered_bottom_padding = MODAL_BOTTOM_PADDING + 1.0;

    app.update(cx, |app, app_cx| {
        app.open_new_connection_directory_dialog(app_cx);
        assert!(
            app.connection_directory_modal.is_some(),
            "应创建新增目录模态框"
        );
        app_cx.notify();
    });
    cx.simulate_resize(gpui::size(gpui::px(1401.0), gpui::px(900.0)));
    cx.simulate_resize(gpui::size(gpui::px(1402.0), gpui::px(900.0)));
    let container = cx
        .debug_bounds("connection-directory-modal-container-shell")
        .expect("应渲染新增目录弹窗");
    let submit = cx
        .debug_bounds("connection-directory-window-submit")
        .expect("应渲染创建目录按钮");
    assert_eq!(
        f32::from(container.bottom() - submit.bottom()),
        bordered_bottom_padding,
        "新建目录按钮与下边框的间距应等于标题到上边框的间距"
    );
    let scroll = cx
        .debug_bounds("connection-directory-window-scroll")
        .expect("应渲染目录表单正文");
    assert_eq!(
        f32::from(scroll.size.height),
        30.0,
        "目录表单正文应完整容纳输入行，不应出现微小滚动"
    );
    let input = cx
        .debug_bounds("connection-directory-name-input")
        .expect("应渲染目录名称输入框");
    assert_eq!(
        f32::from(input.right()),
        f32::from(submit.right()),
        "目录名称输入框右缘应与创建按钮右缘对齐"
    );

    // 空名称提交后追加错误行：外壳只增高一行，按钮仍与下边框保持同一间距。
    let height_without_error = f32::from(container.size.height);
    cx.simulate_click(submit.center(), gpui::Modifiers::default());
    cx.simulate_resize(gpui::size(gpui::px(1402.0), gpui::px(900.0)));
    let container = cx
        .debug_bounds("connection-directory-modal-container-shell")
        .expect("应渲染新增目录弹窗外壳");
    let submit = cx
        .debug_bounds("connection-directory-window-submit")
        .expect("应渲染创建目录按钮");
    assert_eq!(
        f32::from(container.size.height),
        height_without_error + 28.0,
        "错误行应只增加一行提示的高度"
    );
    assert_eq!(
        f32::from(container.bottom() - submit.bottom()),
        bordered_bottom_padding,
        "出现错误行后按钮与下边框的间距不应变化"
    );

    app.update(cx, |app, app_cx| {
        app.close_connection_directory_window();
        app.open_new_ssh_link_dialog(app_cx);
    });
    cx.simulate_resize(gpui::size(gpui::px(1401.0), gpui::px(901.0)));
    let container = cx
        .debug_bounds("connection-link-modal-container-shell")
        .expect("应渲染新增 SSH 链接弹窗");
    let submit = cx
        .debug_bounds("connection-link-window-submit")
        .expect("应渲染创建链接按钮");
    assert_eq!(
        f32::from(container.bottom() - submit.bottom()),
        bordered_bottom_padding,
        "创建链接按钮与下边框的间距应等于标题到上边框的间距"
    );
    let scroll = cx
        .debug_bounds("connection-link-window-scroll")
        .expect("应渲染链接表单正文");
    assert_eq!(
        f32::from(scroll.size.height),
        310.0,
        "SSH 表单正文应完整容纳 7 行输入与凭据说明，不应出现滚动"
    );
    let input = cx
        .debug_bounds("connection-link-private-key-passphrase-input")
        .expect("应渲染私钥口令输入框");
    assert_eq!(
        f32::from(input.right()),
        f32::from(submit.right()),
        "链接表单输入框右缘应与创建按钮右缘对齐"
    );

    // 链接表单校验失败同样只增高一行，按钮保持固定底部间距。
    let height_without_error = f32::from(container.size.height);
    cx.simulate_click(submit.center(), gpui::Modifiers::default());
    cx.simulate_resize(gpui::size(gpui::px(1402.0), gpui::px(901.0)));
    let container = cx
        .debug_bounds("connection-link-modal-container-shell")
        .expect("应渲染新增 SSH 链接弹窗外壳");
    let submit = cx
        .debug_bounds("connection-link-window-submit")
        .expect("应渲染创建链接按钮");
    assert_eq!(
        f32::from(container.size.height),
        height_without_error + 28.0,
        "链接表单错误行应只增加一行提示的高度"
    );
    assert_eq!(
        f32::from(container.bottom() - submit.bottom()),
        bordered_bottom_padding,
        "链接表单出现错误行后按钮与下边框的间距不应变化"
    );

    app.update(cx, |app, _| {
        app.close_connection_link_window();
        app.archive_password_prompt = Some(ArchivePasswordPromptState {
            error: ArchivePasswordError::required("/tmp/secret.zip"),
            input: TextInputState::default(),
            retry_action: ArchivePasswordRetryAction::ReloadArchiveNode {
                source_id: SourceId(1),
            },
            message: None,
        });
    });
    cx.simulate_resize(gpui::size(gpui::px(1401.0), gpui::px(902.0)));
    let container = cx
        .debug_bounds("archive-password-dialog-container-shell")
        .expect("应渲染压缩包密码弹窗");
    let submit = cx
        .debug_bounds("archive-password-submit")
        .expect("应渲染密码弹窗继续按钮");
    assert_eq!(
        f32::from(container.bottom() - submit.bottom()),
        MODAL_BOTTOM_PADDING,
        "密码弹窗按钮与下边框的间距应等于标题到上边框的间距"
    );
    let input = cx
        .debug_bounds("archive-password-input")
        .expect("应渲染密码输入框");
    assert_eq!(
        f32::from(input.right()),
        f32::from(submit.right()),
        "密码输入框右缘应与继续按钮右缘对齐"
    );

    // 追加错误提示后弹窗同步增高，按钮仍保持同一底部间距。
    let height_without_error = f32::from(container.size.height);
    app.update(cx, |app, app_cx| {
        if let Some(prompt) = app.archive_password_prompt.as_mut() {
            prompt.message = Some("密码错误，请重试".to_string());
        }
        app_cx.notify();
    });
    cx.simulate_resize(gpui::size(gpui::px(1401.0), gpui::px(903.0)));
    let container = cx
        .debug_bounds("archive-password-dialog-container-shell")
        .expect("应渲染压缩包密码弹窗外壳");
    let submit = cx
        .debug_bounds("archive-password-submit")
        .expect("应渲染密码弹窗继续按钮");
    assert_eq!(
        f32::from(container.size.height),
        height_without_error + 28.0,
        "密码错误提示应只增加一行提示的高度"
    );
    assert_eq!(
        f32::from(container.bottom() - submit.bottom()),
        MODAL_BOTTOM_PADDING,
        "出现密码错误提示后按钮与下边框的间距不应变化"
    );

    // 主机指纹确认与删除确认同样使用固定底栏，按钮与下边框保持统一间距。
    app.update(cx, |app, app_cx| {
        app.archive_password_prompt = None;
        app.connection_dialog = Some(ConnectionDialogState::ConfirmHostKey(
            ConnectionHostKeyPromptState {
                session_id: 1,
                owner: HostKeyPromptOwner::Terminal { session_id: 1 },
                link_id: 1,
                host: "10.0.0.1".to_string(),
                port: 22,
                fingerprint: "SHA256:abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG".to_string(),
            },
        ));
        app_cx.notify();
    });
    cx.simulate_resize(gpui::size(gpui::px(1401.0), gpui::px(904.0)));
    let container = cx
        .debug_bounds("connection-host-key-dialog-container-shell")
        .expect("应渲染主机指纹确认弹窗");
    let submit = cx
        .debug_bounds("connection-host-key-confirm")
        .expect("应渲染信任并继续按钮");
    assert_eq!(
        f32::from(container.bottom() - submit.bottom()),
        MODAL_BOTTOM_PADDING,
        "主机指纹弹窗按钮与下边框的间距应等于标题到上边框的间距"
    );

    app.update(cx, |app, app_cx| {
        app.connection_dialog = Some(ConnectionDialogState::ConfirmDelete(
            ConnectionDeletePromptState {
                node_id: 1,
                label: "app-01".to_string(),
                is_directory: false,
            },
        ));
        app_cx.notify();
    });
    cx.simulate_resize(gpui::size(gpui::px(1401.0), gpui::px(905.0)));
    let container = cx
        .debug_bounds("connection-delete-dialog-container-shell")
        .expect("应渲染删除确认弹窗");
    let submit = cx
        .debug_bounds("connection-delete-dialog-submit")
        .expect("应渲染确认删除按钮");
    assert_eq!(
        f32::from(container.bottom() - submit.bottom()),
        bordered_bottom_padding,
        "删除确认弹窗按钮与下边框的间距应等于标题到上边框的间距"
    );
}

/// 验证点击链接树空白区域会同时取消节点选中并关闭残留菜单。
#[test]
fn connection_tree_blank_click_clears_selection() {
    let mut app = test_app();
    let link_id = add_test_ssh_link(&mut app);
    app.open_connection_tree_context_menu(link_id, gpui::point(gpui::px(1.0), gpui::px(1.0)));
    assert_eq!(app.selected_connection_node_id, Some(link_id));
    assert!(app.active_menu.is_some());

    assert!(app.clear_connection_tree_selection());
    assert_eq!(app.selected_connection_node_id, None);
    assert!(app.active_menu.is_none());
    assert!(!app.clear_connection_tree_selection());
}

/// 验证应用层拖放动作会移动链接、保留选中状态并持久化目标目录。
#[test]
fn connection_tree_drop_moves_link_to_directory() {
    let mut app = test_app();
    let target = app
        .config
        .connections
        .add_directory(None, "归档目录")
        .unwrap();
    let link_id = add_test_ssh_link(&mut app);

    assert!(app.move_connection_link(link_id, Some(target)));
    assert_eq!(
        app.config.connections.link(link_id).unwrap().parent_id,
        Some(target)
    );
    assert_eq!(app.selected_connection_node_id, Some(link_id));
    assert!(app.placeholder_notice.contains("归档目录"));
}

/// 验证 SFTP 文件行右键菜单展示下载、重命名和删除动作。
#[test]
fn sftp_entry_context_menu_shows_file_actions() {
    let mut app = test_app();
    let link_id = add_test_ssh_link(&mut app);
    let _receiver = insert_test_sftp_session(&mut app, 1, link_id);
    let remote_path = "/home/tester/app.log".to_string();
    if let Some(session) = app.remote_file_sessions.get_mut(&1) {
        session.entries = vec![crate::remote::remote_file::RemoteFileEntry {
            name: "app.log".to_string(),
            path: remote_path.clone(),
            kind: crate::remote::remote_file::RemoteFileEntryKind::RegularFile,
            size: Some(128),
            mtime: None,
            permissions: Some(0o100644),
        }];
    }

    app.open_remote_file_entry_context_menu(
        1,
        remote_path.clone(),
        gpui::point(gpui::px(2.0), gpui::px(3.0)),
    );

    assert!(matches!(
        app.active_menu.as_ref().map(|menu| &menu.kind),
        Some(ActiveMenuKind::RemoteFileEntry { session_id }) if *session_id == 1
    ));
    assert_eq!(
        app.remote_file_sessions
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
        MenuAction::PreviewRemoteFileSelection { session_id } if session_id == 1
    ));
    assert!(matches!(
        entries[1].action,
        MenuAction::DownloadRemoteFileSelection { session_id } if session_id == 1
    ));
    assert!(matches!(
        entries[2].action,
        MenuAction::RenameRemoteFileSelection { session_id } if session_id == 1
    ));
    assert!(matches!(
        entries[3].action,
        MenuAction::DeleteRemoteFileSelection { session_id } if session_id == 1
    ));
    assert!(entries[3].is_danger);
}

/// 验证 Git/SVN 只读会话的右键菜单和动作层都不会生成任何写命令。
#[test]
fn repository_file_actions_hide_and_reject_all_writes() {
    let mut app = test_app();
    let link_id = app
        .config
        .connections
        .add_git_link(
            None,
            "测试仓库",
            crate::remote::connection::GitLinkConfig {
                url: "https://example.com/repo.git".to_string(),
                ..Default::default()
            },
        )
        .unwrap();
    let link = app.config.connections.link(link_id).unwrap().clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut session = crate::remote::remote_file::RemoteFileSessionState::connecting(
        9,
        &link,
        crate::remote::remote_file::RemoteFileBackend::Git,
        sender,
    );
    let remote_path = "/README.md".to_string();
    session.status = crate::remote::remote_file::RemoteFileStatus::Connected;
    session.entries = vec![crate::remote::remote_file::RemoteFileEntry {
        name: "README.md".to_string(),
        path: remote_path.clone(),
        kind: crate::remote::remote_file::RemoteFileEntryKind::RegularFile,
        size: Some(128),
        mtime: None,
        permissions: Some(0o644),
    }];
    app.remote_file_sessions.insert(9, session);

    app.open_remote_file_entry_context_menu(
        9,
        remote_path,
        gpui::point(gpui::px(2.0), gpui::px(3.0)),
    );
    let entries = app.active_menu_entries();
    assert_eq!(entries.len(), 2);
    assert!(matches!(
        entries[0].action,
        MenuAction::PreviewRemoteFileSelection { session_id: 9 }
    ));
    assert!(matches!(
        entries[1].action,
        MenuAction::DownloadRemoteFileSelection { session_id: 9 }
    ));
    assert!(!app.can_rename_remote_file_selection(9));
    assert!(!app.can_delete_remote_file_selection(9));

    app.upload_remote_files(9, vec![PathBuf::from("README.md")]);
    app.open_remote_file_rename_dialog(9);
    app.request_delete_remote_file_entry(9);
    assert!(matches!(
        receiver.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
}

/// 验证 Git 版本菜单按 worker 返回的稳定 ID 切换，并标记当前分支。
#[test]
fn repository_version_menu_dispatches_selected_reference() {
    let mut app = test_app();
    let link_id = add_test_ssh_link(&mut app);
    let receiver = insert_test_sftp_session(&mut app, 12, link_id);
    let session = app.remote_file_sessions.get_mut(&12).unwrap();
    session.backend = crate::remote::remote_file::RemoteFileBackend::Git;
    session.capabilities = session.backend.capabilities();
    session.repository_versions = vec![crate::remote::remote_file::RepositoryVersion {
        id: "refs/remotes/origin/main".to_string(),
        label: "main（默认分支）".to_string(),
        kind: crate::remote::remote_file::RepositoryVersionKind::GitBranch,
    }];
    session.selected_repository_version = Some("refs/remotes/origin/main".to_string());

    app.open_repository_version_menu(12, gpui::point(gpui::px(3.0), gpui::px(4.0)));
    let entries = app.active_menu_entries();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].is_selected);
    let action = entries[0].action.clone();
    app.handle_menu_action(action);

    assert!(matches!(
        receiver.try_recv(),
        Ok(crate::remote::remote_file::RemoteFileCommand::SwitchRepositoryVersion { version_id })
            if version_id == "refs/remotes/origin/main"
    ));
}

/// 验证右键已选集合内文件时保留多选，方便从菜单下载多个文件。
#[test]
fn sftp_entry_context_menu_preserves_existing_multi_selection() {
    let mut app = test_app();
    let link_id = add_test_ssh_link(&mut app);
    let _receiver = insert_test_sftp_session(&mut app, 1, link_id);
    let first_path = "/home/tester/app.log".to_string();
    let second_path = "/home/tester/error.log".to_string();
    if let Some(session) = app.remote_file_sessions.get_mut(&1) {
        session.entries = vec![
            crate::remote::remote_file::RemoteFileEntry {
                name: "app.log".to_string(),
                path: first_path.clone(),
                kind: crate::remote::remote_file::RemoteFileEntryKind::RegularFile,
                size: Some(128),
                mtime: None,
                permissions: Some(0o100644),
            },
            crate::remote::remote_file::RemoteFileEntry {
                name: "error.log".to_string(),
                path: second_path.clone(),
                kind: crate::remote::remote_file::RemoteFileEntryKind::RegularFile,
                size: Some(256),
                mtime: None,
                permissions: Some(0o100644),
            },
        ];
        session.selected_paths.insert(first_path.clone());
        session.selected_paths.insert(second_path.clone());
    }

    app.open_remote_file_entry_context_menu(
        1,
        second_path,
        gpui::point(gpui::px(2.0), gpui::px(3.0)),
    );

    let selected_paths = &app
        .remote_file_sessions
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

    app.create_remote_file_tab_for_session(1);
    app.create_remote_file_tab_for_session(2);

    assert_eq!(app.tabs.len(), 2);
    assert!(matches!(
        app.tabs[0].kind,
        TabKind::RemoteFileManager { session_id } if session_id == 1
    ));
    assert!(matches!(
        app.tabs[1].kind,
        TabKind::RemoteFileManager { session_id } if session_id == 2
    ));
    assert_eq!(app.tabs[0].title, "SFTP - 测试服务器");
    assert_eq!(app.tabs[1].title, "SFTP - 测试服务器");
    assert_eq!(app.active_tab_id, Some(app.tabs[1].id));
}

/// 验证关闭 SFTP 文件管理标签会断开并清理对应会话。
#[test]
fn close_sftp_tab_disconnects_session() {
    let mut app = test_app();
    let link_id = add_test_ssh_link(&mut app);
    let receiver = insert_test_sftp_session(&mut app, 1, link_id);
    app.tabs = vec![ArgusTab {
        id: 1,
        title: "SFTP - 测试服务器".to_string(),
        kind: TabKind::RemoteFileManager { session_id: 1 },
    }];
    app.next_tab_id = 2;
    app.active_tab_id = Some(1);

    app.close_tab(1);

    assert!(app.remote_file_sessions.is_empty());
    assert!(matches!(
        receiver.try_recv(),
        Ok(crate::remote::remote_file::RemoteFileCommand::Disconnect)
    ));
    assert!(app.tabs.is_empty(), "关闭最后一个标签后不应保留占位标签");
}

/// 验证 SFTP 删除入口只允许普通文件和目录，避免误删符号链接等特殊条目。
#[test]
fn sftp_delete_selection_rejects_special_entries() {
    let mut app = test_app();
    let link_id = add_test_ssh_link(&mut app);
    let _receiver = insert_test_sftp_session(&mut app, 1, link_id);
    let remote_path = "/home/tester/current".to_string();
    if let Some(session) = app.remote_file_sessions.get_mut(&1) {
        session.entries = vec![crate::remote::remote_file::RemoteFileEntry {
            name: "current".to_string(),
            path: remote_path.clone(),
            kind: crate::remote::remote_file::RemoteFileEntryKind::Symlink,
            size: None,
            mtime: None,
            permissions: None,
        }];
        session.selected_paths.insert(remote_path);
    }

    assert!(!app.can_delete_remote_file_selection(1));
    app.request_delete_remote_file_entry(1);

    assert!(app.remote_file_dialog.is_none());
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
    if let Some(session) = app.remote_file_sessions.get_mut(&1) {
        session.entries = vec![crate::remote::remote_file::RemoteFileEntry {
            name: "app.log".to_string(),
            path: remote_path.clone(),
            kind: crate::remote::remote_file::RemoteFileEntryKind::RegularFile,
            size: Some(128),
            mtime: None,
            permissions: Some(0o100644),
        }];
        session.selected_paths.insert(remote_path);
        session.status = crate::remote::remote_file::RemoteFileStatus::Transferring;
    }

    assert!(!app.can_download_remote_file_selection(1));
    assert!(!app.can_rename_remote_file_selection(1));
    assert!(!app.can_delete_remote_file_selection(1));
}

/// 验证密码占位的加密压缩包节点不展示分析右键菜单，也不参与搜索多选。
#[test]
fn source_tree_context_menu_rejects_password_required_archive() {
    let mut app = test_app();
    let mut registry = SourceRegistry::new();
    let archive_id = registry.allocate_id();
    registry.insert_node(SourceTreeNode {
        id: archive_id,
        parent_id: None,
        depth: 0,
        label: "secret.zip".to_string(),
        kind: SourceKind::ArchivePasswordRequired,
        location: SourceLocation::LocalPath(PathBuf::from("secret.zip")),
        metadata: SourceMetadata {
            size: Some(1024),
            children_loaded: false,
            is_loading: false,
            message: Some("压缩包已加密，选择后输入密码展开".to_string()),
        },
        selected: false,
        expanded: false,
    });
    registry.rebuild_all_indices();
    app.source_registry = registry;

    assert!(!app.is_source_selectable_for_search_selection(archive_id));
    app.open_source_tree_context_menu(archive_id, gpui::point(gpui::px(1.0), gpui::px(1.0)));

    assert!(app.active_menu.is_none());
    assert!(app.placeholder_notice.contains("不是可分析的日志候选"));
}

/// 验证目录节点也能显示 Jstack 与 Runtime 分析入口。
#[test]
fn source_tree_context_menu_shows_analysis_actions_for_archive_directory() {
    let (mut app, archive_dir_id, _, _) = app_with_loaded_archive_directory();

    app.open_source_tree_context_menu(archive_dir_id, gpui::point(gpui::px(1.0), gpui::px(1.0)));

    assert!(matches!(
        app.active_menu.as_ref().map(|menu| &menu.kind),
        Some(ActiveMenuKind::SourceTree { source_id }) if *source_id == archive_dir_id
    ));
    assert_eq!(app.active_menu_entries().len(), 3);
    assert!(matches!(
        app.active_menu_entries()[0].action,
        MenuAction::OpenJstackAnalysis { source_id } if source_id == archive_dir_id
    ));
    assert!(matches!(
        app.active_menu_entries()[1].action,
        MenuAction::OpenRuntimeAnalysis { source_id } if source_id == archive_dir_id
    ));
    assert!(matches!(
        app.active_menu_entries()[2].action,
        MenuAction::AddSourceToAssistant {
            source_id,
            session_id: None
        } if source_id == archive_dir_id
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

/// 验证 Jstack 右键目录时，目录本身作为独立目标交给后台递归展开。
#[test]
fn jstack_context_archive_directory_collects_loaded_descendants() {
    let (mut app, archive_dir_id, _, _) = app_with_loaded_archive_directory();

    let source_ids = app.jstack_source_ids_for_context(archive_dir_id);
    let targets = app.jstack_targets_from_source_ids(&source_ids);

    assert_eq!(source_ids, vec![archive_dir_id]);
    assert_eq!(targets.len(), 1);
    assert!(matches!(targets[0].location, SourceLocation::LocalPath(_)));
    assert_eq!(targets[0].label, "runtime");
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
    app.log_search.message = Some("搜索结果面板仍在展示".to_string());

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
    assert!(!app.should_show_log_search_results());
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

/// 验证关闭规则编辑器会把草稿写回配置并刷新应用快照。
#[test]
fn closing_jstack_filter_rule_editor_commits_draft() {
    let mut app = test_app();
    app.update_jstack_thread_filter_rules(Vec::new());
    app.jstack_filter_rule_editor_draft = Some(JstackFilterRuleDraft {
        rule_index: None,
        kind: JstackThreadFilterRuleKind::ThreadName,
        input: TextInputState::from_value("Attach Listener".to_string()),
        discard_on_close: false,
    });

    app.close_jstack_filter_rule_editor();

    let rules = &app.config.log_display.jstack_thread_filter_rules;
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].pattern, "Attach Listener");
    assert_eq!(rules[0].kind, JstackThreadFilterRuleKind::ThreadName);
    assert!(app.jstack_filter_rule_editor_draft.is_none());
    assert!(!app.is_jstack_filter_rule_editor_open);
    assert_eq!(app.placeholder_notice, "过滤规则已保存");
    assert_eq!(
        app.applied_jstack_thread_filter_rules,
        app.config.log_display.jstack_thread_filter_rules
    );
}

/// 验证关闭规则编辑器时会替换指定索引的已有规则，而不是追加新规则。
#[test]
fn closing_jstack_filter_rule_editor_replaces_existing_rule() {
    let mut app = test_app();
    app.update_jstack_thread_filter_rules(vec![JstackThreadFilterRule {
        enabled: false,
        kind: JstackThreadFilterRuleKind::ThreadName,
        pattern: "old".to_string(),
    }]);
    app.jstack_filter_rule_editor_draft = Some(JstackFilterRuleDraft {
        rule_index: Some(0),
        kind: JstackThreadFilterRuleKind::StackSegment,
        input: TextInputState::from_value("Unsafe.park".to_string()),
        discard_on_close: false,
    });

    app.close_jstack_filter_rule_editor();

    let rules = &app.config.log_display.jstack_thread_filter_rules;
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].kind, JstackThreadFilterRuleKind::StackSegment);
    assert_eq!(rules[0].pattern, "Unsafe.park");
}

/// 验证取消规则编辑后关闭编辑器不会修改配置。
#[test]
fn discarding_jstack_filter_rule_editor_skips_commit() {
    let mut app = test_app();
    let rules_before = app.config.log_display.jstack_thread_filter_rules.clone();
    app.jstack_filter_rule_editor_draft = Some(JstackFilterRuleDraft {
        rule_index: None,
        kind: JstackThreadFilterRuleKind::ThreadName,
        input: TextInputState::from_value("Ghost".to_string()),
        discard_on_close: false,
    });

    app.discard_jstack_filter_rule_editor();
    assert!(
        app.jstack_filter_rule_editor_draft
            .as_ref()
            .expect("草稿应存在")
            .discard_on_close
    );
    app.close_jstack_filter_rule_editor();

    assert_eq!(
        app.config.log_display.jstack_thread_filter_rules,
        rules_before
    );
    assert_eq!(app.placeholder_notice, "已取消过滤规则编辑");
}

/// 验证草稿内容为空时关闭编辑器不会保存规则。
#[test]
fn closing_jstack_filter_rule_editor_with_empty_pattern_skips_save() {
    let mut app = test_app();
    let rules_before = app.config.log_display.jstack_thread_filter_rules.clone();
    app.jstack_filter_rule_editor_draft = Some(JstackFilterRuleDraft {
        rule_index: None,
        kind: JstackThreadFilterRuleKind::StackSegment,
        input: TextInputState::from_value("   \n\t ".to_string()),
        discard_on_close: false,
    });

    app.close_jstack_filter_rule_editor();

    assert_eq!(
        app.config.log_display.jstack_thread_filter_rules,
        rules_before
    );
    assert_eq!(app.placeholder_notice, "规则内容为空，未保存");
}

/// 验证切换规则启用状态会持久化并同步命中徽标数据。
#[test]
fn toggling_jstack_thread_filter_rule_updates_enabled_state() {
    let mut app = test_app();
    let was_enabled = app.config.log_display.jstack_thread_filter_rules[0].enabled;

    app.toggle_jstack_thread_filter_rule(0);

    assert_eq!(
        app.config.log_display.jstack_thread_filter_rules[0].enabled,
        !was_enabled
    );
    assert_eq!(
        app.applied_jstack_thread_filter_rules,
        app.config.log_display.jstack_thread_filter_rules
    );

    // 越界索引只提示，不改变任何规则。
    let rules_before = app.config.log_display.jstack_thread_filter_rules.clone();
    app.toggle_jstack_thread_filter_rule(99);
    assert_eq!(
        app.config.log_display.jstack_thread_filter_rules,
        rules_before
    );
    assert_eq!(app.placeholder_notice, "未找到要切换的过滤规则");
}

/// 验证删除规则会更新配置和命中徽标数据，越界索引安全提示。
#[test]
fn deleting_jstack_thread_filter_rule_removes_entry() {
    let mut app = test_app();
    let before_count = app.config.log_display.jstack_thread_filter_rules.len();

    app.delete_jstack_thread_filter_rule(0);

    assert_eq!(
        app.config.log_display.jstack_thread_filter_rules.len(),
        before_count - 1
    );
    assert_eq!(app.placeholder_notice, "过滤规则已删除");

    let rules_before = app.config.log_display.jstack_thread_filter_rules.clone();
    app.delete_jstack_thread_filter_rule(99);
    assert_eq!(
        app.config.log_display.jstack_thread_filter_rules,
        rules_before
    );
    assert_eq!(app.placeholder_notice, "未找到要删除的过滤规则");
}

/// 验证规则变化时刷新会重建可见行缓存并重新计算命中徽标。
#[test]
fn refresh_jstack_thread_filter_rebuilds_only_when_rules_change() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let targets = app.jstack_targets_from_source_ids(&[app_log_id]);
    let (analysis_id, generation) = app
        .create_jstack_analysis_tab_state(targets)
        .expect("应能创建 Jstack 分析 tab");
    let snapshot = crate::analysis::jstack::parse_jstack_snapshot(
        SourceId(1),
        "001.log",
        "/tmp/001.log",
        r#""Attach Listener" #1
   java.lang.Thread.State: RUNNABLE
"business-worker" #2
   java.lang.Thread.State: RUNNABLE
"#,
    );
    let result = crate::analysis::jstack::build_analysis_result(vec![snapshot], Vec::new(), 1);
    app.apply_jstack_analysis_result(analysis_id, generation, result);
    // 同步应用快照，模拟规则已经应用过的状态。
    app.applied_jstack_thread_filter_rules =
        app.config.log_display.jstack_thread_filter_rules.clone();

    // 规则未变化：刷新只重算徽标，不重建可见行缓存。
    app.jstack_analyses
        .get_mut(&analysis_id)
        .expect("应存在分析状态")
        .visible_row_indices
        .clear();
    app.refresh_jstack_thread_filter();
    assert!(
        app.jstack_analyses[&analysis_id]
            .visible_row_indices
            .is_empty(),
        "规则未变化时不应重建可见行缓存"
    );

    // 规则变化：刷新必须重建可见行缓存并同步快照。
    app.update_jstack_thread_filter_rules(Vec::new());
    assert!(
        !app.jstack_analyses[&analysis_id]
            .visible_row_indices
            .is_empty(),
        "规则变化后应重建可见行缓存"
    );
    assert_eq!(
        app.applied_jstack_thread_filter_rules,
        app.config.log_display.jstack_thread_filter_rules
    );
}

/// 验证设置页打开时新完成的分析会刷新规则命中徽标。
#[test]
fn applying_jstack_result_refreshes_rule_hit_counts_when_settings_open() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let targets = app.jstack_targets_from_source_ids(&[app_log_id]);
    let (analysis_id, generation) = app
        .create_jstack_analysis_tab_state(targets)
        .expect("应能创建 Jstack 分析 tab");
    app.is_settings_modal_open = true;
    // 默认规则第三条为 Attach Listener 线程名规则。
    let attach_listener_rule_index = app
        .config
        .log_display
        .jstack_thread_filter_rules
        .iter()
        .position(|rule| rule.pattern == "Attach Listener")
        .expect("默认规则应包含 Attach Listener");
    let snapshot = crate::analysis::jstack::parse_jstack_snapshot(
        SourceId(1),
        "001.log",
        "/tmp/001.log",
        r#""Attach Listener" #1
   java.lang.Thread.State: RUNNABLE
"business-worker" #2
   java.lang.Thread.State: RUNNABLE
"#,
    );
    let result = crate::analysis::jstack::build_analysis_result(vec![snapshot], Vec::new(), 1);

    app.apply_jstack_analysis_result(analysis_id, generation, result);

    assert_eq!(
        app.jstack_filter_rule_hit_counts[attach_listener_rule_index], 1,
        "Attach Listener 线程应命中默认线程名规则"
    );
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

/// 验证 Runtime 右键目录时，目录本身作为递归展开的分析目标。
#[test]
fn runtime_context_archive_directory_collects_loaded_descendant_files() {
    let (mut app, archive_dir_id, _, _) = app_with_loaded_archive_directory();

    let targets = app.runtime_targets_for_context(archive_dir_id);

    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].source_id, archive_dir_id);
    assert!(matches!(
        targets[0].kind,
        RuntimeAnalysisTargetKind::Directory
    ));
}

/// 验证创建 Runtime 分析 tab 会复用空 tab 并写入加载状态。
#[test]
fn creating_runtime_analysis_tab_reuses_empty_tab() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let error_log_id = source_id_by_label(&app, "error.log");
    let targets = app.runtime_targets_from_source_ids(&[app_log_id, error_log_id]);
    app.log_search.message = Some("搜索结果面板仍在展示".to_string());

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
    assert!(!app.should_show_log_search_results());
}

/// 验证切换到已经存在的分析标签也会关闭日志搜索结果面板。
#[test]
fn activating_existing_analysis_tab_closes_search_results_panel() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    app.select_source(app_log_id);
    let log_tab_id = app.active_tab_id.expect("应有激活标签");

    let targets = app.jstack_targets_from_source_ids(&[app_log_id]);
    app.create_jstack_analysis_tab_state(targets)
        .expect("应能创建 Jstack 分析 tab");
    let analysis_tab_id = app.active_tab_id.expect("应有激活标签");
    app.activate_tab(log_tab_id);
    app.log_search.message = Some("搜索结果面板仍在展示".to_string());

    app.activate_tab(analysis_tab_id);

    assert_eq!(app.active_tab_id, Some(analysis_tab_id));
    assert!(!app.should_show_log_search_results());
    assert!(app.log_search.message.is_none());
    assert_eq!(app.placeholder_notice, "已切换到 Jstack分析");
}

/// 验证关闭当前日志标签并自动落到分析标签时也会关闭日志搜索结果面板。
#[test]
fn closing_active_tab_into_analysis_closes_search_results_panel() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    app.select_source(app_log_id);
    let log_tab_id = app.active_tab_id.expect("应有激活标签");

    let targets = app.jstack_targets_from_source_ids(&[app_log_id]);
    app.create_jstack_analysis_tab_state(targets)
        .expect("应能创建 Jstack 分析 tab");
    let analysis_tab_id = app.active_tab_id.expect("应有激活标签");
    app.activate_tab(log_tab_id);
    app.log_search.message = Some("搜索结果面板仍在展示".to_string());

    app.close_tab(log_tab_id);

    assert_eq!(app.active_tab_id, Some(analysis_tab_id));
    assert!(!app.should_show_log_search_results());
    assert!(app.log_search.message.is_none());
}

/// 验证关闭其他标签并保留分析标签时会关闭日志搜索结果面板。
#[test]
fn keeping_analysis_tab_closes_search_results_panel() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    app.select_source(app_log_id);
    let log_tab_id = app.active_tab_id.expect("应有激活标签");

    let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
    app.create_runtime_analysis_tab_state(targets)
        .expect("应能创建 Runtime 分析 tab");
    let analysis_tab_id = app.active_tab_id.expect("应有激活标签");
    app.activate_tab(log_tab_id);
    app.log_search.message = Some("搜索结果面板仍在展示".to_string());

    app.close_other_tabs(analysis_tab_id);

    assert_eq!(app.active_tab_id, Some(analysis_tab_id));
    assert!(!app.should_show_log_search_results());
    assert!(app.log_search.message.is_none());
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

/// 构造时间选择器字段测试用数字按键，`key_char` 与 key 一致，覆盖真实键盘的数字直接输入。
fn picker_time_digit_keystroke(digit: &str) -> Keystroke {
    Keystroke {
        key: digit.to_string(),
        modifiers: gpui::Modifiers::default(),
        key_char: Some(digit.to_string()),
    }
}

/// 构造时间选择器字段测试用命名按键（方向键、回车等），此类按键没有字符输入。
fn picker_time_named_keystroke(key: &str) -> Keystroke {
    Keystroke {
        key: key.to_string(),
        modifiers: gpui::Modifiers::default(),
        key_char: None,
    }
}

/// 验证时间选择器时分秒字段支持直接键入数字：一位即时提交、两位夹取并自动推进，
/// 方向键等价步进、退格只清草稿、回车结束字段编辑。
#[test]
fn runtime_time_picker_time_fields_accept_direct_digit_input() {
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
    app.begin_runtime_picker_time_edit(analysis_id, RuntimeDateTimePart::Hour);

    // 一位数字按原值提交；字段编辑期间过滤输入框不再显示光标。
    app.handle_runtime_picker_time_key(analysis_id, &picker_time_digit_keystroke("8"), None);
    let state = app
        .runtime_analysis_state(analysis_id)
        .expect("应存在 Runtime 分析状态");
    assert_eq!(state.filter_start_time_input.value, "2026-06-25 08:25:03");
    assert!(!state.filter_start_time_input.is_focused);
    assert_eq!(
        state
            .picker_time_edit
            .as_ref()
            .map(|edit| (edit.part, edit.digits.as_str())),
        Some((RuntimeDateTimePart::Hour, "8"))
    );

    // 两位数字按字段上限夹取，并自动跳到分钟字段。
    app.handle_runtime_picker_time_key(analysis_id, &picker_time_digit_keystroke("5"), None);
    let state = app
        .runtime_analysis_state(analysis_id)
        .expect("应存在 Runtime 分析状态");
    assert_eq!(state.filter_start_time_input.value, "2026-06-25 23:25:03");
    assert_eq!(
        state
            .picker_time_edit
            .as_ref()
            .map(|edit| (edit.part, edit.digits.as_str())),
        Some((RuntimeDateTimePart::Minute, ""))
    );

    // 分钟继续键入两位数字后自动跳到秒字段。
    app.handle_runtime_picker_time_key(analysis_id, &picker_time_digit_keystroke("4"), None);
    app.handle_runtime_picker_time_key(analysis_id, &picker_time_digit_keystroke("5"), None);
    let state = app
        .runtime_analysis_state(analysis_id)
        .expect("应存在 Runtime 分析状态");
    assert_eq!(state.filter_start_time_input.value, "2026-06-25 23:45:03");
    assert_eq!(
        state.picker_time_edit.as_ref().map(|edit| edit.part),
        Some(RuntimeDateTimePart::Second)
    );

    // 秒字段两位数字输入完成后仍停留在秒字段，重新开始新的数字草稿；超出上限按 59 夹取。
    app.handle_runtime_picker_time_key(analysis_id, &picker_time_digit_keystroke("7"), None);
    app.handle_runtime_picker_time_key(analysis_id, &picker_time_digit_keystroke("2"), None);
    let state = app
        .runtime_analysis_state(analysis_id)
        .expect("应存在 Runtime 分析状态");
    assert_eq!(state.filter_start_time_input.value, "2026-06-25 23:45:59");
    assert_eq!(
        state
            .picker_time_edit
            .as_ref()
            .map(|edit| (edit.part, edit.digits.as_str())),
        Some((RuntimeDateTimePart::Second, ""))
    );

    // 退格只清除数字草稿，不改动已提交的数值。
    app.handle_runtime_picker_time_key(analysis_id, &picker_time_digit_keystroke("7"), None);
    app.handle_runtime_picker_time_key(
        analysis_id,
        &picker_time_named_keystroke("backspace"),
        None,
    );
    let state = app
        .runtime_analysis_state(analysis_id)
        .expect("应存在 Runtime 分析状态");
    assert_eq!(state.filter_start_time_input.value, "2026-06-25 23:45:07");
    assert_eq!(
        state
            .picker_time_edit
            .as_ref()
            .map(|edit| edit.digits.as_str()),
        Some("")
    );

    // 上方向键等价于点击 + 按钮，并保持草稿为空。
    app.handle_runtime_picker_time_key(analysis_id, &picker_time_named_keystroke("arrowup"), None);
    let state = app
        .runtime_analysis_state(analysis_id)
        .expect("应存在 Runtime 分析状态");
    assert_eq!(state.filter_start_time_input.value, "2026-06-25 23:45:08");

    // 回车结束字段编辑，后续数字键不再改写过滤值。
    app.handle_runtime_picker_time_key(analysis_id, &picker_time_named_keystroke("enter"), None);
    let state = app
        .runtime_analysis_state(analysis_id)
        .expect("应存在 Runtime 分析状态");
    assert!(state.picker_time_edit.is_none());
    app.handle_runtime_picker_time_key(analysis_id, &picker_time_digit_keystroke("9"), None);
    assert_eq!(
        app.runtime_analysis_state(analysis_id)
            .expect("应存在 Runtime 分析状态")
            .filter_start_time_input
            .value,
        "2026-06-25 23:45:08"
    );
}

/// 验证切换或关闭时间选择器会清理字段直接输入状态，数字键不再改写过滤值。
#[test]
fn runtime_time_picker_switching_or_closing_clears_direct_input_state() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
    let (analysis_id, _) = app
        .create_runtime_analysis_tab_state(targets)
        .expect("应能创建 Runtime 分析 tab");

    app.open_runtime_time_picker(analysis_id, RuntimeFilterInputKind::StartTime);
    app.begin_runtime_picker_time_edit(analysis_id, RuntimeDateTimePart::Minute);
    assert_eq!(
        app.runtime_analysis_state(analysis_id)
            .expect("应存在 Runtime 分析状态")
            .picker_time_edit
            .as_ref()
            .map(|edit| edit.part),
        Some(RuntimeDateTimePart::Minute)
    );

    // 切换到结束时间选择器：上一个字段的直接输入状态应被清理。
    app.open_runtime_time_picker(analysis_id, RuntimeFilterInputKind::EndTime);
    assert!(
        app.runtime_analysis_state(analysis_id)
            .expect("应存在 Runtime 分析状态")
            .picker_time_edit
            .is_none()
    );

    // 关闭选择器：同样清理字段直接输入状态。
    app.begin_runtime_picker_time_edit(analysis_id, RuntimeDateTimePart::Hour);
    assert!(app.close_runtime_time_picker(analysis_id));
    assert!(
        app.runtime_analysis_state(analysis_id)
            .expect("应存在 Runtime 分析状态")
            .picker_time_edit
            .is_none()
    );
}

/// 构造处于 Ready 状态的 Runtime 分析页，供日期时间选择器的真实渲染测试使用。
fn runtime_analysis_ready_for_picker_test(app: &mut ArgusApp, start_time: &str) -> usize {
    let app_log_id = source_id_by_label(app, "app.log");
    let targets = app.runtime_targets_from_source_ids(&[app_log_id]);
    let (analysis_id, _) = app
        .create_runtime_analysis_tab_state(targets)
        .expect("应能创建 Runtime 分析 tab");
    let mut request = parse_runtime_request_text(
        SourceId(1),
        "100&alice&_api_one&1000&0&0.log",
        "/tmp/one.log",
        "10 0 0 0 0 select * from users",
    )
    .expect("应能解析 Runtime 测试日志");
    request.index = 0;
    let result = build_runtime_analysis_result(vec![request], Vec::new(), 1, false);
    let state = app
        .runtime_analysis_state_mut(analysis_id)
        .expect("应存在 Runtime 分析状态");
    state.task_state = RuntimeAnalysisTaskState::Ready(Arc::new(result));
    state.filter_start_time_input.value = start_time.to_string();
    analysis_id
}

/// 验证点击时间选择器时分秒字段后浮层获得原生键盘焦点，直接键入数字即可改写时间过滤值。
#[gpui::test]
fn runtime_time_picker_field_click_accepts_keyboard_digits(cx: &mut gpui::TestAppContext) {
    let (app, cx) = cx.add_window_view(|_, _| app_with_placeholder_sources());
    cx.simulate_resize(gpui::size(gpui::px(1600.0), gpui::px(900.0)));
    let analysis_id = app.update(cx, |app, _| {
        let analysis_id = runtime_analysis_ready_for_picker_test(app, "2026-06-25 14:25:03");
        app.open_runtime_time_picker(analysis_id, RuntimeFilterInputKind::StartTime);
        analysis_id
    });
    // 再铺一帧，确保打开的时间选择器浮层进入最新渲染帧再读取字段边界。
    cx.simulate_resize(gpui::size(gpui::px(1601.0), gpui::px(900.0)));

    // 首个 Runtime 分析页 ID 固定为 1，与组件内的动态 debug 选择器一致（同 tab_bar 测试约定）。
    let hour_field_bounds = cx
        .debug_bounds("runtime-time-field-1-StartTime-Hour")
        .expect("应渲染时间选择器小时字段");
    cx.simulate_click(hour_field_bounds.center(), gpui::Modifiers::default());
    cx.update(|window, app_cx| {
        let focus_handle = app
            .read(app_cx)
            .input_focus_handles
            .as_ref()
            .expect("主窗口焦点句柄应已初始化")
            .runtime_time_picker
            .clone();
        assert!(
            focus_handle.is_focused(window),
            "点击时分秒字段后浮层应获得原生键盘焦点"
        );
    });

    cx.simulate_keystrokes("8");
    app.update(cx, |app, _| {
        let state = app
            .runtime_analysis_state(analysis_id)
            .expect("应存在 Runtime 分析状态");
        assert_eq!(state.filter_start_time_input.value, "2026-06-25 08:25:03");
        assert_eq!(
            state.picker_time_edit.as_ref().map(|edit| edit.part),
            Some(RuntimeDateTimePart::Hour)
        );
    });

    // 回车结束字段编辑并把真实焦点交还时间过滤输入框。
    cx.simulate_keystrokes("enter");
    cx.update(|window, app_cx| {
        let app_ref = app.read(app_cx);
        assert!(
            app_ref
                .runtime_analysis_state(analysis_id)
                .expect("应存在 Runtime 分析状态")
                .picker_time_edit
                .is_none(),
            "回车应结束字段直接输入状态"
        );
        let filter_focus_handle = app_ref
            .input_focus_handles
            .as_ref()
            .expect("主窗口焦点句柄应已初始化")
            .runtime_filter_start_time
            .clone();
        assert!(
            filter_focus_handle.is_focused(window),
            "回车后焦点应交还时间过滤输入框"
        );
    });
}

/// 验证窗口变窄时过滤栏输入框按比例收缩，时间选择器浮层始终对齐对应输入框。
///
/// 回归背景：过滤栏输入框按固定宽度布局时，窗口过窄会把玻璃板右缘顶出窗口；
/// 改为可收缩布局后，浮层必须跟随实测偏移而不是固定常量。
#[gpui::test]
fn runtime_time_picker_tracks_shrunken_filter_inputs(cx: &mut gpui::TestAppContext) {
    let (app, cx) = cx.add_window_view(|_, _| app_with_placeholder_sources());
    let analysis_id = app.update(cx, |app, _| {
        let analysis_id = runtime_analysis_ready_for_picker_test(app, "2026-06-25 14:25:03");
        app.open_runtime_time_picker(analysis_id, RuntimeFilterInputKind::StartTime);
        analysis_id
    });
    // 连续铺三帧：窄窗口测量 → 回填实测偏移 → 浮层按实测偏移定位。
    cx.simulate_resize(gpui::size(gpui::px(1000.0), gpui::px(800.0)));
    cx.simulate_resize(gpui::size(gpui::px(1001.0), gpui::px(800.0)));
    cx.simulate_resize(gpui::size(gpui::px(1002.0), gpui::px(800.0)));

    let input_bounds = cx
        .debug_bounds("runtime-filter-StartTime-input")
        .expect("应渲染开始时间输入框");
    let panel_bounds = cx
        .debug_bounds("runtime-time-picker-panel")
        .expect("应渲染时间选择器浮层");
    let offset = f32::from(panel_bounds.left()) - f32::from(input_bounds.left());
    assert!(
        offset.abs() < 1.0,
        "浮层应与开始时间输入框左对齐，实际偏移 {offset:.1}px"
    );
    app.update(cx, |app, _| {
        assert!(
            app.runtime_analysis_state(analysis_id)
                .expect("应存在 Runtime 分析状态")
                .time_input_offsets
                .is_some(),
            "窄窗口渲染后应回填实测输入框偏移"
        );
    });
}

/// 验证 Runtime 过滤状态单独占一行展示，不再与输入框挤在同一行被截断。
#[gpui::test]
fn runtime_filter_status_renders_on_its_own_row(cx: &mut gpui::TestAppContext) {
    let (app, cx) = cx.add_window_view(|_, _| app_with_placeholder_sources());
    cx.simulate_resize(gpui::size(gpui::px(1600.0), gpui::px(900.0)));
    app.update(cx, |app, _| {
        let analysis_id = runtime_analysis_ready_for_picker_test(app, "2026-06-25 14:25:03");
        // 输入框与已应用过滤保持一致，状态行展示完整过滤条件摘要。
        let state = app
            .runtime_analysis_state_mut(analysis_id)
            .expect("应存在 Runtime 分析状态");
        state.filter_keyword_input.value = "timeout".to_string();
        state.applied_filter_keyword = "timeout".to_string();
    });
    // 再铺一帧，确保更新后的过滤栏进入最新渲染帧再读取行边界。
    cx.simulate_resize(gpui::size(gpui::px(1601.0), gpui::px(900.0)));

    let input_row = cx
        .debug_bounds("runtime-filter-input-row")
        .expect("应渲染 Runtime 过滤输入行");
    let status_row = cx
        .debug_bounds("runtime-filter-status")
        .expect("应渲染 Runtime 过滤状态行");
    assert!(
        status_row.top() >= input_row.bottom(),
        "过滤状态应换行显示在过滤输入行下方"
    );
    assert!(
        status_row.size.width >= input_row.size.width - gpui::px(1.0),
        "过滤状态行应占满整行宽度，长过滤条件才有换行空间"
    );
}

/// 验证 SSH 终端断开后整个内容区显示连接断开面板与重连按钮。
#[gpui::test]
fn disconnected_terminal_shows_reconnect_panel(cx: &mut gpui::TestAppContext) {
    let (app, cx) = cx.add_window_view(|_, _| test_app());
    let link_id = app.update(cx, |app, _| add_test_ssh_link(app));
    app.update(cx, |app, _| {
        insert_test_terminal_session(app, 7, link_id);
        let session = app
            .terminal_sessions
            .get_mut(&7)
            .expect("应存在测试终端会话");
        session.status = crate::remote::terminal::TerminalStatus::Disconnected;
        session.command_sender = None;
        session.message = Some("SSH 连接已断开".to_string());
        app.workspace = Workspace::Connections;
        app.tabs = vec![ArgusTab {
            id: 4,
            title: "测试服务器".to_string(),
            kind: TabKind::SshTerminal { session_id: 7 },
        }];
        app.active_tab_id = Some(4);
    });
    cx.simulate_resize(gpui::size(gpui::px(1400.0), gpui::px(900.0)));

    assert!(
        cx.debug_bounds("ssh-terminal-connection-lost").is_some(),
        "断开的终端应整窗显示连接断开面板"
    );
    assert!(
        cx.debug_bounds("ssh-terminal-connection-lost-reconnect")
            .is_some(),
        "连接断开面板应提供重连按钮"
    );
}

/// 验证远程文件管理会话断开后整个内容区显示连接断开面板与重连按钮。
#[gpui::test]
fn disconnected_remote_file_session_shows_reconnect_panel(cx: &mut gpui::TestAppContext) {
    let (app, cx) = cx.add_window_view(|_, _| test_app());
    let link_id = app.update(cx, |app, _| add_test_ssh_link(app));
    app.update(cx, |app, _| {
        let _receiver = insert_test_sftp_session(app, 9, link_id);
        let session = app
            .remote_file_sessions
            .get_mut(&9)
            .expect("应存在测试文件管理会话");
        session.status = crate::remote::remote_file::RemoteFileStatus::Disconnected;
        session.command_sender = None;
        session.message = Some("SFTP 连接已断开".to_string());
        app.workspace = Workspace::Connections;
        app.tabs = vec![ArgusTab {
            id: 5,
            title: "SFTP 测试服务器".to_string(),
            kind: TabKind::RemoteFileManager { session_id: 9 },
        }];
        app.active_tab_id = Some(5);
    });
    cx.simulate_resize(gpui::size(gpui::px(1400.0), gpui::px(900.0)));

    assert!(
        cx.debug_bounds("remote-file-connection-lost").is_some(),
        "断开的文件管理会话应整窗显示连接断开面板"
    );
    assert!(
        cx.debug_bounds("remote-file-connection-lost-reconnect")
            .is_some(),
        "连接断开面板应提供重连按钮"
    );
}

/// 验证终端重连复用同一会话 ID 重建 worker 与会话状态，并如实提示正在重连。
#[gpui::test]
fn terminal_reconnect_rebuilds_disconnected_session(cx: &mut gpui::TestAppContext) {
    let app = cx.new(|_| test_app());
    let link_id = app.update(cx, |app, _| add_test_ssh_link(app));

    app.update(cx, |app, app_cx| {
        insert_test_terminal_session(app, 7, link_id);
        let session = app
            .terminal_sessions
            .get_mut(&7)
            .expect("应存在测试终端会话");
        session.status = crate::remote::terminal::TerminalStatus::Disconnected;
        session.command_sender = None;
        session.message = Some("SSH 连接已断开".to_string());

        app.reconnect_terminal_session(7, app_cx);

        let session = app.terminal_sessions.get(&7).expect("重连后会话应保留");
        assert_eq!(
            session.status,
            crate::remote::terminal::TerminalStatus::Connecting,
            "重连后会话应回到连接中状态"
        );
        assert!(session.command_sender.is_some(), "重连应建立新的命令通道");
        assert!(app.placeholder_notice.contains("正在重连"));
    });
}

/// 验证链接不存在时终端重连如实失败，不静默把会话重置为连接中。
#[gpui::test]
fn terminal_reconnect_reports_missing_link(cx: &mut gpui::TestAppContext) {
    let app = cx.new(|_| test_app());
    let link_id = app.update(cx, |app, _| add_test_ssh_link(app));

    app.update(cx, |app, app_cx| {
        insert_test_terminal_session(app, 7, link_id);
        let session = app
            .terminal_sessions
            .get_mut(&7)
            .expect("应存在测试终端会话");
        session.status = crate::remote::terminal::TerminalStatus::Disconnected;
        session.command_sender = None;
        // 模拟链接已被删除：会话仍指向不存在的链接节点。
        session.link_id = 9999;

        app.reconnect_terminal_session(7, app_cx);

        let session = app.terminal_sessions.get(&7).expect("会话应保留");
        assert_eq!(
            session.status,
            crate::remote::terminal::TerminalStatus::Disconnected
        );
        assert!(session.command_sender.is_none());
        assert!(app.placeholder_notice.contains("未找到 SSH 链接"));
    });
}

/// 验证远程文件管理会话重连复用同一会话 ID 重建 worker 与会话状态。
#[gpui::test]
fn remote_file_reconnect_rebuilds_disconnected_session(cx: &mut gpui::TestAppContext) {
    let app = cx.new(|_| test_app());
    let link_id = app.update(cx, |app, _| add_test_ssh_link(app));

    app.update(cx, |app, app_cx| {
        let _receiver = insert_test_sftp_session(app, 9, link_id);
        let session = app
            .remote_file_sessions
            .get_mut(&9)
            .expect("应存在测试文件管理会话");
        session.status = crate::remote::remote_file::RemoteFileStatus::Disconnected;
        session.command_sender = None;
        session.message = Some("SFTP 连接已断开".to_string());

        app.reconnect_remote_file_session(9, app_cx);

        let session = app.remote_file_sessions.get(&9).expect("重连后会话应保留");
        assert_eq!(
            session.status,
            crate::remote::remote_file::RemoteFileStatus::Connecting,
            "重连后会话应回到连接中状态"
        );
        assert!(session.command_sender.is_some(), "重连应建立新的命令通道");
        assert!(app.placeholder_notice.contains("正在重连"));
    });
}

/// 验证终端按键按远端输入模式编码：控制键、编辑键区与应用光标键模式下的方向键。
#[gpui::test]
fn terminal_keys_follow_remote_input_mode(cx: &mut gpui::TestAppContext) {
    let app = cx.new(|_| test_app());
    let link_id = app.update(cx, |app, _| add_test_ssh_link(app));

    app.update(cx, |app, app_cx| {
        let receiver = insert_test_terminal_session_with_commands(app, 7, link_id);
        app.tabs = vec![ArgusTab {
            id: 4,
            title: "测试服务器".to_string(),
            kind: TabKind::SshTerminal { session_id: 7 },
        }];
        app.active_tab_id = Some(4);

        // 普通方向键：CSI；PageDown 与 Ctrl+A 分别走编辑键区与控制字节。
        app.handle_terminal_key(
            &terminal_keystroke("up", None, gpui::Modifiers::default()),
            app_cx,
        );
        app.handle_terminal_key(
            &terminal_keystroke("pagedown", None, gpui::Modifiers::default()),
            app_cx,
        );
        app.handle_terminal_key(
            &terminal_keystroke(
                "a",
                Some("a"),
                gpui::Modifiers {
                    control: true,
                    ..Default::default()
                },
            ),
            app_cx,
        );
        assert_eq!(
            drain_terminal_input(&receiver),
            b"\x1b[A\x1b[6~\x01".to_vec()
        );

        // 远端开启应用光标键模式后：未带修饰的方向键切换为 SS3，带修饰仍使用 CSI。
        app.terminal_sessions
            .get_mut(&7)
            .expect("应存在测试终端会话")
            .process_output(b"\x1b[?1h");
        app.handle_terminal_key(
            &terminal_keystroke("up", None, gpui::Modifiers::default()),
            app_cx,
        );
        app.handle_terminal_key(
            &terminal_keystroke(
                "up",
                None,
                gpui::Modifiers {
                    control: true,
                    ..Default::default()
                },
            ),
            app_cx,
        );
        assert_eq!(drain_terminal_input(&receiver), b"\x1bOA\x1b[1;5A".to_vec());

        // 平台快捷键保持本地，不会发送到远端。
        app.handle_terminal_key(
            &terminal_keystroke(
                "c",
                Some("c"),
                gpui::Modifiers {
                    platform: true,
                    ..Default::default()
                },
            ),
            app_cx,
        );
        assert!(drain_terminal_input(&receiver).is_empty());
    });
}

/// 验证鼠标转发仅在远端开启鼠标模式时生效，且按下/释放成对上报。
#[gpui::test]
fn terminal_mouse_forwarding_follows_remote_mode(cx: &mut gpui::TestAppContext) {
    use crate::remote::terminal_input::{TerminalKeyModifiers, TerminalMouseButton};

    let app = cx.new(|_| test_app());
    let link_id = app.update(cx, |app, _| add_test_ssh_link(app));

    app.update(cx, |app, _| {
        let receiver = insert_test_terminal_session_with_commands(app, 7, link_id);
        app.tabs = vec![ArgusTab {
            id: 4,
            title: "测试服务器".to_string(),
            kind: TabKind::SshTerminal { session_id: 7 },
        }];
        app.active_tab_id = Some(4);

        // 远端未开启鼠标模式：本地选择语义，不产生字节。
        assert!(!app.terminal_forwards_mouse(7, false));
        app.forward_terminal_mouse_press(
            7,
            TerminalMouseButton::Left,
            TerminalKeyModifiers::default(),
            10,
            5,
        );
        assert!(drain_terminal_input(&receiver).is_empty());

        // 开启 VT200 + SGR 后：按下与释放按 SGR 编码成对上报，Shift 保留本地语义。
        app.terminal_sessions
            .get_mut(&7)
            .expect("应存在测试终端会话")
            .process_output(b"\x1b[?1000h\x1b[?1006h");
        assert!(app.terminal_forwards_mouse(7, false));
        assert!(!app.terminal_forwards_mouse(7, true));

        app.forward_terminal_mouse_press(
            7,
            TerminalMouseButton::Left,
            TerminalKeyModifiers::default(),
            10,
            5,
        );
        app.forward_terminal_mouse_release(
            7,
            TerminalMouseButton::Left,
            TerminalKeyModifiers::default(),
            10,
            5,
        );
        // 未经历过按下转发的释放事件不应回发。
        app.forward_terminal_mouse_release(
            7,
            TerminalMouseButton::Left,
            TerminalKeyModifiers::default(),
            10,
            5,
        );
        assert_eq!(
            drain_terminal_input(&receiver),
            b"\x1b[<0;10;5M\x1b[<0;10;5m".to_vec()
        );
    });
}

/// 验证焦点事件仅在远端开启 1004 时上报，且首次未聚焦不发送失焦。
#[gpui::test]
fn terminal_focus_events_follow_remote_mode(cx: &mut gpui::TestAppContext) {
    let app = cx.new(|_| test_app());
    let link_id = app.update(cx, |app, _| add_test_ssh_link(app));

    app.update(cx, |app, _| {
        let receiver = insert_test_terminal_session_with_commands(app, 7, link_id);

        // 首次上报只记录状态：初始未聚焦不应发送失焦事件。
        app.report_terminal_focus(7, false);
        app.terminal_sessions
            .get_mut(&7)
            .expect("应存在测试终端会话")
            .process_output(b"\x1b[?1004h");

        app.report_terminal_focus(7, true);
        app.report_terminal_focus(7, true);
        app.report_terminal_focus(7, false);
        assert_eq!(drain_terminal_input(&receiver), b"\x1b[I\x1b[O".to_vec());
    });
}

/// 验证「Option 键作为 Meta」设置可切换并持久化到设置文件。
#[test]
fn terminal_option_as_meta_toggle_persists() {
    let mut app = test_app();
    assert!(!app.config.terminal.option_as_meta);

    app.toggle_terminal_option_as_meta();
    assert!(app.config.terminal.option_as_meta);
    let (loaded, _warnings) = ConfigManager::load_from_path(app.config_manager.settings_path())
        .expect("应能重新加载设置");
    assert!(loaded.terminal.option_as_meta);

    app.toggle_terminal_option_as_meta();
    assert!(!app.config.terminal.option_as_meta);
}

/// 验证已连接的终端视图正常渲染正文，并在聚焦与模式切换时保持稳定。
#[gpui::test]
fn connected_terminal_view_renders_body(cx: &mut gpui::TestAppContext) {
    let (app, cx) = cx.add_window_view(|_, _| test_app());
    let link_id = app.update(cx, |app, _| add_test_ssh_link(app));
    app.update(cx, |app, _| {
        let _receiver = insert_test_terminal_session_with_commands(app, 7, link_id);
        app.terminal_sessions
            .get_mut(&7)
            .expect("应存在测试终端会话")
            .process_output(b"\x1b[?1004h\x1b[?1000h\x1b[?1006hconnected\r\n");
        app.tabs = vec![ArgusTab {
            id: 4,
            title: "测试服务器".to_string(),
            kind: TabKind::SshTerminal { session_id: 7 },
        }];
        app.active_tab_id = Some(4);
    });
    cx.simulate_resize(gpui::size(gpui::px(1200.0), gpui::px(800.0)));

    let body_bounds = cx
        .debug_bounds("ssh-terminal-body")
        .expect("已连接的终端应渲染正文区域");
    // 点击正文聚焦终端：触发焦点事件上报与下一帧回写路径。
    cx.simulate_click(body_bounds.center(), gpui::Modifiers::default());
    cx.update(|window, app_cx| {
        let is_focused = app
            .read(app_cx)
            .input_focus_handles
            .as_ref()
            .is_some_and(|handles| handles.terminal.is_focused(window));
        assert!(is_focused, "点击终端正文后应获得键盘焦点");
    });
    // 回写发生在下一帧，连续铺两帧保证调度回调已经执行。
    cx.simulate_resize(gpui::size(gpui::px(1201.0), gpui::px(800.0)));
    cx.simulate_resize(gpui::size(gpui::px(1202.0), gpui::px(800.0)));

    app.update(cx, |app, _| {
        assert_eq!(
            app.terminal_sessions
                .get(&7)
                .expect("应存在测试终端会话")
                .reported_focus,
            Some(true),
            "聚焦终端后应记录并上报焦点状态"
        );
    });
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
    let tab_id = app.active_tab_id.expect("应有激活标签");

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

/// 验证线程状态筛选全不选时过滤统计仍按全部行计算，与历史口径一致。
#[test]
fn jstack_filtered_row_count_survives_empty_active_states() {
    let snapshot = crate::analysis::jstack::parse_jstack_snapshot(
        SourceId(1),
        "001.log",
        "/tmp/001.log",
        r#""Attach Listener" #1
   java.lang.Thread.State: RUNNABLE
"business-worker" #2
   java.lang.Thread.State: RUNNABLE
"#,
    );
    let result = crate::analysis::jstack::build_analysis_result(vec![snapshot], Vec::new(), 1);
    let filter =
        crate::analysis::jstack::JstackThreadFilter::from_rules(&[JstackThreadFilterRule {
            enabled: true,
            kind: JstackThreadFilterRuleKind::ThreadName,
            pattern: "listener".to_string(),
        }]);
    let mut state = JstackAnalysisState {
        generation: 1,
        active_states: BTreeSet::new(),
        is_thread_filter_enabled: true,
        thread_name_selection: None,
        thread_name_selection_drag: None,
        selected_cell_key: None,
        visible_row_indices: Vec::new(),
        filtered_row_count: 0,
        row_scroll: UniformListScrollHandle::new(),
        task_state: JstackAnalysisTaskState::Ready(result),
        cancel_token: None,
    };

    state.rebuild_visible_row_cache(&filter);

    assert_eq!(state.filtered_row_count, 1);
    assert!(state.visible_row_indices.is_empty());
}

/// 验证 Jstack 头部过滤控件固定独占一行，窗口变窄时版式稳定且不溢出内容玻璃板右边缘。
#[gpui::test]
fn jstack_header_controls_wrap_within_content_panel(cx: &mut gpui::TestAppContext) {
    let (app, cx) = cx.add_window_view(|_, _| app_with_placeholder_sources());
    app.update(cx, |app, _| {
        let app_log_id = source_id_by_label(app, "app.log");
        let targets = app.jstack_targets_from_source_ids(&[app_log_id]);
        app.create_jstack_analysis_tab_state(targets)
            .expect("应能创建 Jstack 分析 tab");
    });

    cx.simulate_resize(gpui::size(gpui::px(1600.0), gpui::px(900.0)));
    let wide_controls = cx
        .debug_bounds("jstack-header-filter-controls")
        .expect("应渲染头部过滤控件");
    let wide_panel = cx
        .debug_bounds("window-content-panel")
        .expect("应渲染内容玻璃板");
    assert!(
        wide_controls.right() <= wide_panel.right() + gpui::px(0.5),
        "宽窗口下过滤控件应位于玻璃板内：controls={wide_controls:?}, panel={wide_panel:?}"
    );

    cx.simulate_resize(gpui::size(gpui::px(760.0), gpui::px(900.0)));
    let narrow_controls = cx
        .debug_bounds("jstack-header-filter-controls")
        .expect("应渲染头部过滤控件");
    let narrow_panel = cx
        .debug_bounds("window-content-panel")
        .expect("应渲染内容玻璃板");
    assert!(
        (narrow_controls.top() - wide_controls.top()).abs() <= gpui::px(0.5),
        "过滤控件应固定独占一行，窗口变窄时位置不变：wide={wide_controls:?}, narrow={narrow_controls:?}"
    );
    assert!(
        narrow_controls.right() <= narrow_panel.right() + gpui::px(0.5),
        "窄窗口下过滤控件不应溢出玻璃板右边缘：controls={narrow_controls:?}, panel={narrow_panel:?}"
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

/// 验证 Shift 范围多选只纳入日志候选节点，普通压缩包节点不再打断也不进入选择集合。
#[test]
fn shift_range_selection_skips_non_log_candidates() {
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
    let archive_id = registry.allocate_id();
    registry.insert_node(SourceTreeNode {
        id: archive_id,
        parent_id: Some(root_id),
        depth: 1,
        label: "002.zip".to_string(),
        kind: SourceKind::Unsupported("ZIP".to_string()),
        location: SourceLocation::LocalPath(PathBuf::from("logs/002.zip")),
        metadata: SourceMetadata {
            size: Some(20),
            children_loaded: true,
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
        BTreeSet::from([first_id, last_id])
    );
}

/// 验证来源树过滤态下，Shift 范围多选只纳入命中的日志候选节点。
#[test]
fn source_tree_filter_shift_range_selection_keeps_only_log_candidates() {
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
        ("thread001.log", SourceKind::LogFile),
        ("thread002.zip", SourceKind::Unsupported("ZIP".to_string())),
        ("thread003.log", SourceKind::LogFile),
    ];
    let mut ids = Vec::new();
    for (label, kind) in source_specs {
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
                children_loaded: true,
                is_loading: false,
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

    assert_eq!(
        app.selected_search_source_ids,
        BTreeSet::from_iter([ids[0], ids[2]])
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
    let tab_id = app.active_tab_id.expect("应有激活标签");

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

/// 验证正式启动时不创建任何标签页，内容区按未选择日志渲染空态。
#[test]
fn new_app_starts_without_tabs() {
    let app = test_app();

    assert!(app.tabs.is_empty(), "启动时不应创建占位标签");
    assert_eq!(app.active_tab_id, None);
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
    app.jstack_filter_rule_editor_draft = Some(JstackFilterRuleDraft {
        rule_index: None,
        kind: JstackThreadFilterRuleKind::ThreadName,
        input: TextInputState {
            value: "草稿".to_string(),
            cursor: 2,
            selection_anchor: None,
            marked_range: None,
            selection_drag: None,
            is_focused: true,
            scroll_handle: gpui::ScrollHandle::new(),
        },
        discard_on_close: false,
    });

    app.close_settings_modal();

    assert!(!app.is_settings_modal_open);
    assert!(
        !app.jstack_filter_rule_editor_draft
            .as_ref()
            .expect("草稿应保留")
            .input
            .is_focused
    );
}

/// 验证同一日志来源重复点击时复用已有标签页。
#[test]
fn selecting_same_log_reuses_existing_tab() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");

    app.select_source(app_log_id);
    let tab_id = app.active_tab_id.expect("应有激活标签");
    app.select_source(app_log_id);

    assert_eq!(app.active_tab_id, Some(tab_id));
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

    let tab_id = app.active_tab_id.expect("应有激活标签");
    app.focus_log_text_view(tab_id);
    assert!(app.is_active_log_view_focused());

    app.close_tab(tab_id);
    assert!(!app.is_active_log_view_focused());
}

/// 验证日志搜索以主窗口内模态框形态打开与关闭，不再创建独立窗口。
#[gpui::test]
fn log_search_opens_and_closes_as_main_window_modal(cx: &mut gpui::TestAppContext) {
    let app = cx.new(|_| app_with_placeholder_sources());
    let app_log_id = app.read_with(cx, |app, _| source_id_by_label(app, "app.log"));

    app.update(cx, |app, app_cx| {
        app.select_source(app_log_id);
        assert!(!app.log_search.is_dialog_open);

        app.open_log_search_dialog(app_cx);
        assert!(app.log_search.is_dialog_open, "搜索应以对话框形态打开");
        assert!(
            app.log_search.search_view.is_some(),
            "应创建主窗口内搜索模态框子视图"
        );
        assert!(
            app.log_search.keyword_input.is_focused,
            "打开后关键字输入框应获得焦点"
        );

        // 再次打开只聚焦已有对话框，不重建子视图。
        let existing_view = app.log_search.search_view.clone();
        app.open_log_search_dialog(app_cx);
        assert_eq!(app.log_search.search_view, existing_view);

        app.close_log_search_dialog();
        assert!(!app.log_search.is_dialog_open);
        assert!(
            app.log_search.search_view.is_none(),
            "关闭后应移除模态框子视图"
        );
        assert!(!app.log_search.keyword_input.is_focused);
    });
}

/// 验证日志行号打点在同一行重复点击时会添加再移除。
#[test]
fn toggling_log_line_marker_adds_and_removes_line() {
    let mut app = test_app();
    let tab_id = push_test_log_tab(&mut app, "app.log");

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
    let tab_id = push_test_log_tab(&mut app, "app.log");
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
    let tab_id = app.active_tab_id.expect("应有激活标签");
    app.toggle_log_line_marker(tab_id, 2);
    app.close_tab(tab_id);

    assert!(
        app.log_tab_view_state(tab_id).is_none(),
        "关闭标签后应释放该标签的视图状态（含打点）"
    );
    assert!(app.tabs.is_empty());
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
    let app_tab_id = app.active_tab_id.expect("应有激活标签");
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

    assert_eq!(app.active_tab_id, Some(app_tab_id));
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
    let app_tab_id = app.active_tab_id.expect("应有激活标签");
    app.select_source(error_log_id);
    app.selected_search_source_ids = BTreeSet::from([error_log_id, nested_log_id]);

    app.activate_tab(app_tab_id);

    assert_eq!(app.active_tab_id, Some(app_tab_id));
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

/// 验证关闭当前标签后会激活相邻标签，关闭最后一个标签后不再保留占位标签。
#[test]
fn close_tab_activates_neighbor_and_drops_last_tab() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let error_log_id = source_id_by_label(&app, "error.log");
    app.select_source(app_log_id);
    app.select_source(error_log_id);
    let error_tab_id = app.active_tab_id.expect("应有激活标签");

    app.close_tab(error_tab_id);
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.active_tab_title(), "app.log");

    let last_tab_id = app.active_tab_id.expect("应有激活标签");
    app.close_tab(last_tab_id);
    assert!(app.tabs.is_empty(), "关闭最后一个标签后不应保留占位标签");
    assert_eq!(app.active_tab_id, None);
    assert!(matches!(app.active_tab_kind(), TabKind::Empty));
}

/// 验证标签右键菜单会记录目标标签和窗口锚点。
#[test]
fn tab_context_menu_records_target_tab_and_anchor() {
    let mut app = test_app();
    let target_tab_id = push_test_log_tab(&mut app, "app.log");
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

    let entries = app.active_menu_entries();
    assert_eq!(entries.len(), 3);
    assert!(
        entries.iter().all(|entry| entry.icon.is_none()),
        "标签页右键菜单不应显示图标"
    );
}

/// 验证关闭其他标签只保留目标标签并激活它。
#[test]
fn close_other_tabs_keeps_target_tab_active() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let error_log_id = source_id_by_label(&app, "error.log");

    app.select_source(app_log_id);
    let app_tab_id = app.active_tab_id.expect("应有激活标签");
    app.select_source(error_log_id);
    app.close_other_tabs(app_tab_id);

    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.active_tab_id, Some(app_tab_id));
    assert_eq!(app.active_tab_title(), "app.log");
}

/// 验证保存智能助手设置会写回配置文件，而不是只校验后返回成功。
#[gpui::test]
fn save_ai_settings_persists_configuration(cx: &mut gpui::TestAppContext) {
    let settings_path = isolated_test_dir("save-ai-settings").join("settings.toml");
    let app =
        cx.new(|_| ArgusApp::new_with_config_manager(ConfigManager::new(settings_path.clone())));
    let mut config = app.read_with(cx, |app, _| app.config.ai.clone());
    config.max_retry_attempts = 3;

    app.update(cx, |app, app_cx| {
        app.save_ai_settings(config, None, app_cx)
            .expect("有效配置应保存成功");
    });

    let saved = ConfigManager::new(settings_path).load();
    assert_eq!(saved.ai.max_retry_attempts, 3);
}

/// 验证来源侧栏入口直接创建助手页签并渲染会话页面，不弹出旧问题对话框。
#[gpui::test]
fn assistant_toolbar_click_opens_embedded_chat_tab(cx: &mut gpui::TestAppContext) {
    let mut app = test_app();
    app.is_source_panel_collapsed = false;
    let width = app.current_source_panel_width();
    app.source_panel_animation_from_width = width;
    app.source_panel_animation_to_width = width;
    let (app, cx) = cx.add_window_view(|_, _| app);

    let button = cx
        .debug_bounds("source-smart-analysis")
        .expect("来源侧栏应渲染智能助手入口");
    cx.simulate_click(button.center(), gpui::Modifiers::default());

    app.read_with(cx, |app, _| {
        assert_eq!(app.tabs.len(), 1);
        assert!(matches!(app.active_tab_kind(), TabKind::Assistant { .. }));
    });
    assert!(
        cx.debug_bounds("assistant-tab-root").is_some(),
        "点击入口后应直接在主内容区渲染智能助手页面"
    );
    let usage = cx
        .debug_bounds("assistant-token-usage")
        .expect("标题栏右侧应显示 Token 使用量");
    let composer = cx
        .debug_bounds("assistant-composer")
        .expect("应渲染底部输入区");
    let textarea = cx
        .debug_bounds("textarea-assistant-message-input")
        .expect("应渲染助手输入框");
    let model_selector = cx
        .debug_bounds("assistant-model-selector")
        .expect("输入框底部应显示模型选择");
    assert!(
        usage.bottom() <= composer.top(),
        "Token 使用量应位于标题栏，而不是输入区：usage={usage:?}, composer={composer:?}"
    );
    assert!(
        model_selector.top() >= textarea.top()
            && model_selector.bottom() <= textarea.bottom()
            && model_selector.left() >= textarea.left()
            && model_selector.right() <= textarea.right(),
        "模型选择应位于输入框内部底部：model={model_selector:?}, textarea={textarea:?}"
    );
}

/// 验证每次点击智能助手入口都会新增独立页签，并持有各自的会话实体。
#[gpui::test]
fn opening_assistant_tabs_creates_independent_sessions(cx: &mut gpui::TestAppContext) {
    let app = cx.new(|_| test_app());
    app.update(cx, |app, app_cx| {
        app.open_assistant_tab(app_cx);
        app.open_assistant_tab(app_cx);
    });

    app.read_with(cx, |app, _| {
        assert_eq!(app.tabs.len(), 2);
        let session_ids = app
            .tabs
            .iter()
            .map(|tab| match tab.kind {
                TabKind::Assistant { session_id } => session_id,
                _ => panic!("智能助手入口只应创建助手标签页"),
            })
            .collect::<Vec<_>>();
        assert_ne!(
            session_ids[0], session_ids[1],
            "每个助手页签必须拥有独立会话"
        );
        assert_eq!(app.assistant_tabs.len(), 2);
        assert!(
            session_ids
                .iter()
                .all(|id| app.assistant_tabs.contains_key(id))
        );
        assert_eq!(app.active_tab_id, Some(session_ids[1]));
        assert_eq!(
            app.tabs
                .iter()
                .map(|tab| tab.title.as_str())
                .collect::<Vec<_>>(),
            vec!["智能助手", "智能助手 2"]
        );
    });

    let first_tab_id = app.read_with(cx, |app, _| app.tabs[0].id);
    app.update(cx, |app, _| app.close_tab(first_tab_id));
    app.read_with(cx, |app, _| {
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(
            app.assistant_tabs.len(),
            1,
            "关闭助手页签应释放对应会话实体"
        );
    });

    app.update(cx, |app, app_cx| app.open_assistant_tab(app_cx));
    app.read_with(cx, |app, _| {
        assert_eq!(
            app.tabs
                .iter()
                .map(|tab| tab.title.as_str())
                .collect::<Vec<_>>(),
            vec!["智能助手 2", "智能助手"],
            "新页签应复用已关闭页签留下的最小序号"
        );
    });
}

/// 验证只有一个智能助手时，来源树菜单直接指向该会话，不展开二级菜单。
#[gpui::test]
fn single_assistant_menu_adds_directly(cx: &mut gpui::TestAppContext) {
    let (app, _path, file_id) = app_with_real_source_file("assistant-add-single");
    let app = cx.new(|_| app);
    let session_id = app.update(cx, |app, app_cx| {
        app.open_assistant_tab(app_cx);
        app.active_tab_id.expect("应创建助手页签")
    });

    app.update(cx, |app, _| {
        app.open_source_tree_context_menu(file_id, gpui::point(gpui::px(1.0), gpui::px(1.0)));
        let add_entry = app
            .active_menu_entries()
            .into_iter()
            .find(|entry| entry.label == "添加到智能助手")
            .expect("来源树菜单应包含添加到智能助手");
        assert!(!add_entry.has_submenu);
        assert!(matches!(
            add_entry.action,
            MenuAction::AddSourceToAssistant {
                source_id,
                session_id: Some(id)
            } if source_id == file_id && id == session_id
        ));
    });
}

/// 验证没有智能助手时，来源树菜单会新建助手并自动插入“@”引用。
#[gpui::test]
fn add_source_to_assistant_creates_tab_and_inserts_mention(cx: &mut gpui::TestAppContext) {
    let (mut app, path, file_id) = app_with_real_source_file("assistant-add-mention");
    app.source_workspace_root = path.parent().map(Path::to_path_buf);
    let app = cx.new(|_| app);

    app.update(cx, |app, app_cx| {
        app.add_source_to_assistant(file_id, None, None, app_cx);
    });

    app.read_with(cx, |app, app_cx| {
        assert_eq!(app.tabs.len(), 1);
        assert!(matches!(app.active_tab_kind(), TabKind::Assistant { .. }));
        let session_id = match app.tabs[0].kind {
            TabKind::Assistant { session_id } => session_id,
            _ => unreachable!(),
        };
        let tab = app
            .assistant_tabs
            .get(&session_id)
            .expect("应创建助手会话")
            .read(app_cx);
        assert!(tab.input_text_for_test().contains("@app.log"));
        assert_eq!(tab.selected_mention_paths_for_test(), vec!["app.log"]);
    });
}

/// 验证存在多个智能助手时，来源树菜单提供二级选择并只写入目标会话。
#[gpui::test]
fn multiple_assistants_offer_picker_for_add_source(cx: &mut gpui::TestAppContext) {
    let (mut app, path, file_id) = app_with_real_source_file("assistant-add-picker");
    app.source_workspace_root = path.parent().map(Path::to_path_buf);
    let app = cx.new(|_| app);
    app.update(cx, |app, app_cx| {
        app.open_assistant_tab(app_cx);
        app.open_assistant_tab(app_cx);
    });
    let session_ids = app.read_with(cx, |app, _| {
        app.tabs
            .iter()
            .filter_map(|tab| match tab.kind {
                TabKind::Assistant { session_id } => Some(session_id),
                _ => None,
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(session_ids.len(), 2);

    app.update(cx, |app, app_cx| {
        app.open_source_tree_context_menu(file_id, gpui::point(gpui::px(1.0), gpui::px(1.0)));
        let add_entry = app
            .active_menu_entries()
            .into_iter()
            .find(|entry| entry.label == "添加到智能助手")
            .expect("来源树菜单应包含添加到智能助手");
        assert!(matches!(
            add_entry.action,
            MenuAction::OpenAssistantPicker { source_id } if source_id == file_id
        ));
        assert!(add_entry.has_submenu, "多个助手时应展示二级菜单箭头");
        app.open_assistant_picker_menu(file_id);
        assert!(matches!(
            app.active_menu.as_ref().map(|menu| &menu.kind),
            Some(ActiveMenuKind::SourceTreeAssistantPicker { source_id }) if *source_id == file_id
        ));
        assert_eq!(
            app.active_menu_entries()
                .iter()
                .map(|entry| entry.label.as_str())
                .collect::<Vec<_>>(),
            vec!["智能助手", "智能助手 2"]
        );
        app.add_source_to_assistant(file_id, Some(session_ids[0]), None, app_cx);
    });

    app.read_with(cx, |app, app_cx| {
        let first = app
            .assistant_tabs
            .get(&session_ids[0])
            .unwrap()
            .read(app_cx);
        let second = app
            .assistant_tabs
            .get(&session_ids[1])
            .unwrap()
            .read(app_cx);
        assert!(first.input_text_for_test().contains("@app.log"));
        assert!(second.input_text_for_test().is_empty());
    });
}

/// 验证关闭全部标签后不保留任何标签页。
#[test]
fn close_all_tabs_removes_all_tabs() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");

    app.select_source(app_log_id);
    app.close_all_tabs();

    assert!(app.tabs.is_empty(), "关闭全部标签后不应保留占位标签");
    assert_eq!(app.active_tab_id, None);
    assert!(matches!(app.active_tab_kind(), TabKind::Empty));
}

/// 验证标签溢出菜单项点击后会激活目标标签并关闭菜单。
#[test]
fn overflow_menu_action_activates_tab_and_closes_menu() {
    let mut app = app_with_placeholder_sources();
    let app_log_id = source_id_by_label(&app, "app.log");
    let error_log_id = source_id_by_label(&app, "error.log");

    app.select_source(app_log_id);
    let app_tab_id = app.active_tab_id.expect("应有激活标签");
    app.select_source(error_log_id);
    app.open_tab_overflow_menu(gpui::point(gpui::px(200.0), gpui::px(40.0)));
    app.handle_menu_action(MenuAction::ActivateTab { tab_id: app_tab_id });

    assert_eq!(app.active_tab_id, Some(app_tab_id));
    assert_eq!(app.active_tab_title(), "app.log");
    assert!(app.active_menu.is_none());
}

/// 验证来源加载失败和带警告的成功加载都会写入来源加载提示。
///
/// 回归背景：加载失败只写入 `placeholder_notice`，界面没有任何渲染入口，
/// 用户只看到来源树回到“暂无日志来源”，误以为没有反应。
#[gpui::test]
fn source_load_result_surfaces_failure_and_warnings(cx: &mut gpui::TestAppContext) {
    let app = cx.new(|_| app_with_placeholder_sources());
    app.update(cx, |app, cx| {
        assert!(app.source_load_notice.is_none());
        let generation = app.source_load_generation;
        app.apply_source_load_result(
            generation,
            Err(anyhow::anyhow!("RAR 成员解压需要 2.8 GB 临时空间")),
            cx,
        );
        let notice = app
            .source_load_notice
            .as_deref()
            .expect("加载失败应写入来源加载提示");
        assert!(
            notice.contains("RAR 成员解压需要 2.8 GB 临时空间"),
            "提示应保留具体失败原因：{notice}"
        );
    });

    let app = cx.new(|_| app_with_placeholder_sources());
    app.update(cx, |app, _| {
        app.source_load_notice = None;
        let scan_result = crate::loader::SourceTreeScanResult {
            registry: placeholder_source_registry(),
            warnings: vec!["1(1).rar!/1/stdout.log 解压失败，已跳过".to_string()],
        };
        assert!(app.apply_load_report(scan_result));
        let notice = app
            .source_load_notice
            .as_deref()
            .expect("带警告的加载应写入来源加载提示");
        assert!(
            notice.contains("1 项文件已跳过"),
            "提示应说明跳过数量：{notice}"
        );
    });
}

/// 验证首次启动（未加载任何来源）默认收起来源侧栏，内容面板撑满窗口并显示快捷入口。
#[gpui::test]
fn welcome_state_hides_sidebar_and_shows_quick_actions(cx: &mut gpui::TestAppContext) {
    use crate::ui::main_window::WINDOW_CONTENT_INSET;

    let (app, cx) = cx.add_window_view(|_, _| test_app());
    cx.simulate_resize(gpui::size(gpui::px(1400.0), gpui::px(900.0)));
    cx.simulate_resize(gpui::size(gpui::px(1401.0), gpui::px(900.0)));

    app.read_with(cx, |app, _| {
        assert!(app.is_log_welcome(), "首次启动没有来源和标签，应处于欢迎态");
        assert!(app.is_source_panel_collapsed, "欢迎态默认收起来源侧栏");
    });
    assert!(
        cx.debug_bounds("log-welcome").is_some(),
        "欢迎态应渲染快捷入口面板"
    );
    assert!(
        cx.debug_bounds("source-resizer-hit-area").is_none(),
        "侧栏收起时不应渲染宽度拖拽命中区"
    );
    let panel = cx
        .debug_bounds("window-content-panel")
        .expect("应渲染内容玻璃板");
    assert_eq!(
        f32::from(panel.left()),
        WINDOW_CONTENT_INSET,
        "侧栏收起后内容面板应从窗口左留白开始"
    );
    assert_eq!(
        f32::from(panel.right()),
        1401.0 - WINDOW_CONTENT_INSET,
        "内容面板应撑满窗口右缘"
    );
}

/// 验证欢迎页快捷入口：打开日志弹出来源选择器，打开链接切换到链接工作区。
#[gpui::test]
fn welcome_quick_actions_open_log_and_links(cx: &mut gpui::TestAppContext) {
    let (app, cx) = cx.add_window_view(|_, _| test_app());
    cx.simulate_resize(gpui::size(gpui::px(1400.0), gpui::px(900.0)));

    let open_sources = cx
        .debug_bounds("log-welcome-open-sources")
        .expect("欢迎面板应提供打开日志入口");
    cx.simulate_click(open_sources.center(), gpui::Modifiers::default());
    app.update(cx, |app, _| {
        assert!(
            app.source_picker_modal.is_some(),
            "点击打开日志应弹出来源选择器"
        );
    });

    // 再次渲染欢迎态并点击打开链接。
    app.update(cx, |app, _| {
        app.source_picker_modal = None;
        app.source_picker.is_open = false;
    });
    cx.simulate_resize(gpui::size(gpui::px(1402.0), gpui::px(900.0)));
    let open_links = cx
        .debug_bounds("log-welcome-open-links")
        .expect("欢迎面板应提供打开链接入口");
    cx.simulate_click(open_links.center(), gpui::Modifiers::default());
    app.update(cx, |app, _| {
        assert_eq!(
            app.workspace,
            Workspace::Connections,
            "打开链接应切换到链接工作区"
        );
        assert!(app.active_menu.is_none(), "打开链接不应再弹出新建链接菜单");
    });
}

/// 验证来源侧栏收起时标题栏图标按钮仍完整展示，只有侧栏自身内容随面板收起。
#[gpui::test]
fn collapsed_source_panel_keeps_title_bar_buttons(cx: &mut gpui::TestAppContext) {
    let (app, cx) = cx.add_window_view(|_, _| test_app());
    cx.simulate_resize(gpui::size(gpui::px(1400.0), gpui::px(900.0)));

    app.read_with(cx, |app, _| {
        assert!(app.is_source_panel_collapsed, "首次启动侧栏默认收起");
    });
    for id in [
        "title-log-analysis",
        "title-connection",
        "title-settings",
        "title-source-toggle",
    ] {
        assert!(
            cx.debug_bounds(id).is_some(),
            "侧栏收起时标题栏按钮 {id} 仍应展示"
        );
    }
    assert!(
        cx.debug_bounds("argus-compact-title-bar").is_some(),
        "侧栏收起时应使用紧凑标题栏"
    );

    let toggle = cx
        .debug_bounds("title-source-toggle")
        .expect("应渲染侧栏开关按钮");
    cx.simulate_click(toggle.center(), gpui::Modifiers::default());
    cx.simulate_resize(gpui::size(gpui::px(1401.0), gpui::px(900.0)));

    app.read_with(cx, |app, _| {
        assert!(!app.is_source_panel_collapsed, "点击后侧栏应展开");
    });
    for id in [
        "title-log-analysis",
        "title-connection",
        "title-settings",
        "title-source-toggle",
    ] {
        assert!(
            cx.debug_bounds(id).is_some(),
            "侧栏展开后标题栏按钮 {id} 仍应展示"
        );
    }
    assert!(
        cx.debug_bounds("argus-split-title-bar").is_some(),
        "侧栏展开后应使用分栏标题栏"
    );
}

/// 验证已加载来源但未打开标签时展示“请选择日志”提示，不再显示欢迎页，并展开目录树。
#[gpui::test]
fn loaded_sources_without_tabs_prompt_tree_selection(cx: &mut gpui::TestAppContext) {
    let (app, cx) = cx.add_window_view(|_, _| app_with_placeholder_sources());
    cx.simulate_resize(gpui::size(gpui::px(1400.0), gpui::px(900.0)));

    app.update(cx, |app, _| {
        let app_log_id = source_id_by_label(app, "app.log");
        app.select_source(app_log_id);
        let tab_id = app.active_tab_id.expect("打开日志后应有激活标签");
        assert!(!app.is_log_source_selection_pending());
        // 模拟单文件加载后的侧栏收起状态：关闭最后一个标签后应按来源浏览态展开。
        app.is_source_panel_collapsed = true;
        app.close_tab(tab_id);
        assert!(app.tabs.is_empty());
        assert!(
            app.is_log_source_selection_pending(),
            "已加载来源但无标签时应进入选择提示态"
        );
        assert!(
            !app.is_source_panel_collapsed,
            "应展开目录树，让用户直接选择日志"
        );
    });
    cx.simulate_resize(gpui::size(gpui::px(1401.0), gpui::px(900.0)));

    assert!(
        cx.debug_bounds("log-welcome").is_none(),
        "已加载来源后不应再展示欢迎页"
    );
    assert!(
        cx.debug_bounds("log-content-empty-state").is_some(),
        "应展示“请选择日志”提示"
    );
    assert!(
        cx.debug_bounds("source-resizer-hit-area").is_some(),
        "目录树展开后应渲染宽度拖拽命中区"
    );
}

/// 验证最大日志大小按 2 倍档位调整，并收敛到可配置上下限。
#[test]
fn max_log_size_steps_between_configured_bounds() {
    use crate::config::app_config::{
        MAX_LOG_SIZE_MB_DEFAULT, MAX_LOG_SIZE_MB_MAX, MAX_LOG_SIZE_MB_MIN,
    };

    let mut app = test_app();
    app.config.loader.max_log_size_mb = MAX_LOG_SIZE_MB_DEFAULT;
    app.increase_max_log_size();
    assert_eq!(
        app.config.loader.max_log_size_mb,
        MAX_LOG_SIZE_MB_DEFAULT * 2
    );
    app.decrease_max_log_size();
    assert_eq!(app.config.loader.max_log_size_mb, MAX_LOG_SIZE_MB_DEFAULT);

    app.config.loader.max_log_size_mb = MAX_LOG_SIZE_MB_MIN;
    app.decrease_max_log_size();
    assert_eq!(app.config.loader.max_log_size_mb, MAX_LOG_SIZE_MB_MIN);

    app.config.loader.max_log_size_mb = MAX_LOG_SIZE_MB_MAX;
    app.increase_max_log_size();
    assert_eq!(app.config.loader.max_log_size_mb, MAX_LOG_SIZE_MB_MAX);
}

/// 验证来源加载提示条在失败/警告时出现，并在清空提示后消失。
#[gpui::test]
fn source_load_notice_banner_is_visible_and_dismissible(cx: &mut gpui::TestAppContext) {
    let (app, cx) = cx.add_window_view(|_, _| app_with_placeholder_sources());
    cx.simulate_resize(gpui::size(gpui::px(1400.0), gpui::px(900.0)));
    // 欢迎态下提示由内容区欢迎面板展示；先打开标签，验证侧栏提示条。
    app.update(cx, |app, _| {
        push_test_log_tab(app, "app.log");
    });
    cx.simulate_resize(gpui::size(gpui::px(1400.0), gpui::px(901.0)));
    assert!(
        cx.debug_bounds("source-load-notice").is_none(),
        "没有加载问题时不显示提示条"
    );

    app.update(cx, |app, cx| {
        let generation = app.source_load_generation;
        app.apply_source_load_result(generation, Err(anyhow::anyhow!("模拟解压失败")), cx);
    });
    cx.simulate_resize(gpui::size(gpui::px(1401.0), gpui::px(900.0)));
    assert!(
        cx.debug_bounds("source-load-notice").is_some(),
        "加载失败时应显示来源加载提示条"
    );

    app.update(cx, |app, _| app.source_load_notice = None);
    cx.simulate_resize(gpui::size(gpui::px(1402.0), gpui::px(900.0)));
    assert!(
        cx.debug_bounds("source-load-notice").is_none(),
        "关闭提示后提示条应消失"
    );
}

/// 验证标签下拉菜单每行右侧都有关闭按钮：点击后关闭对应标签并保持菜单打开。
#[gpui::test]
fn tab_overflow_menu_close_button_closes_tab_and_keeps_menu(cx: &mut gpui::TestAppContext) {
    let (app, cx) = cx.add_window_view(|_, _| app_with_placeholder_sources());
    cx.simulate_resize(gpui::size(gpui::px(1400.0), gpui::px(900.0)));

    app.update(cx, |app, _| {
        app.tabs = vec![
            ArgusTab {
                id: 1,
                title: "thread_101.log".to_string(),
                kind: TabKind::LogSource {
                    source_id: SourceId(101),
                    path: "/tmp/thread_101.log".to_string(),
                },
            },
            ArgusTab {
                id: 2,
                title: "thread_094.log".to_string(),
                kind: TabKind::LogSource {
                    source_id: SourceId(94),
                    path: "/tmp/thread_094.log".to_string(),
                },
            },
            ArgusTab {
                id: 3,
                title: "thread_093.log".to_string(),
                kind: TabKind::LogSource {
                    source_id: SourceId(93),
                    path: "/tmp/thread_093.log".to_string(),
                },
            },
        ];
        app.active_tab_id = Some(1);
        app.open_tab_overflow_menu(gpui::point(gpui::px(200.0), gpui::px(40.0)));
        // 每个条目都携带行尾关闭动作，避免只有部分行能快捷关闭。
        let entries = app.active_menu_entries();
        assert_eq!(entries.len(), 3, "下拉菜单应列出全部标签");
        for (entry, tab_id) in entries.iter().zip([1, 2, 3]) {
            assert_eq!(
                entry.trailing_action,
                Some(MenuAction::CloseTab { tab_id }),
                "标签 {tab_id} 的行尾关闭动作应指向自身"
            );
        }
    });
    // 再铺一帧，让菜单进入最新渲染帧后再读取按钮边界。
    cx.simulate_resize(gpui::size(gpui::px(1401.0), gpui::px(900.0)));

    for selector in [
        "context-menu-trailing-close-tab-1",
        "context-menu-trailing-close-tab-2",
        "context-menu-trailing-close-tab-3",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "下拉菜单每一行右侧都应渲染关闭按钮：缺少 {selector}"
        );
    }

    let close_button = cx
        .debug_bounds("context-menu-trailing-close-tab-3")
        .expect("应渲染第三个标签的关闭按钮");
    cx.simulate_click(close_button.center(), gpui::Modifiers::default());

    app.update(cx, |app, _| {
        assert!(
            app.tabs.iter().all(|tab| tab.id != 3),
            "点击行尾关闭按钮应关闭对应标签"
        );
        assert_eq!(app.tabs.len(), 2, "只应关闭被点击的标签");
        assert_eq!(app.active_tab_id, Some(1), "关闭按钮不应切换当前标签");
        assert!(
            matches!(
                app.active_menu.as_ref().map(|menu| &menu.kind),
                Some(ActiveMenuKind::TabOverflow)
            ),
            "关闭标签后下拉菜单应保持打开，便于连续关闭"
        );
    });

    // 连续关闭剩余标签：菜单持续可用，关闭最后一个标签后菜单自动收起。
    for remaining in [1, 2] {
        cx.simulate_resize(gpui::size(gpui::px(1400.0), gpui::px(900.0)));
        cx.simulate_resize(gpui::size(gpui::px(1401.0), gpui::px(900.0)));
        let selector: &'static str = if remaining == 1 {
            "context-menu-trailing-close-tab-1"
        } else {
            "context-menu-trailing-close-tab-2"
        };
        let close_button = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("剩余标签 {remaining} 仍应有行尾关闭按钮"));
        cx.simulate_click(close_button.center(), gpui::Modifiers::default());
    }

    app.update(cx, |app, _| {
        assert!(app.tabs.is_empty(), "连续关闭后不应残留标签");
        assert_eq!(app.active_tab_id, None);
        assert!(
            app.active_menu.is_none(),
            "没有标签时标签下拉菜单应自动收起"
        );
    });
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

/// 验证旧版 light/system 配置会迁移到对应内置主题文件。
#[test]
fn legacy_theme_modes_resolve_to_builtin_theme_files() {
    let mut app = test_app();

    app.select_theme("light".to_string());
    assert_eq!(app.selected_theme_id, "light.toml");
    assert_eq!(app.config.appearance.theme_mode, "light.toml");

    app.select_theme("system".to_string());
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
    app.toggle_follow_symlinks();
    app.update_settings_quick_keywords("ERROR,WARN,timeout".to_string());
    app.update_jstack_thread_filter_rules(vec![
        JstackThreadFilterRule {
            enabled: true,
            kind: JstackThreadFilterRuleKind::ThreadName,
            pattern: "Attach Listener".to_string(),
        },
        JstackThreadFilterRule {
            enabled: false,
            kind: JstackThreadFilterRuleKind::StackSegment,
            pattern: "Unsafe.park".to_string(),
        },
    ]);

    let (saved, _warnings) =
        ConfigManager::load_from_path(&settings_path).expect("设置变更后应写入配置文件");

    assert_eq!(saved.appearance.theme_mode, "dark.toml");
    assert_eq!(saved.appearance.log_content_font_size, 14.0);
    assert_eq!(saved.loader.max_archive_depth, 3);
    assert!(saved.loader.follow_symlinks);
    assert_eq!(saved.log_search.quick_keywords, "ERROR,WARN,timeout");
    assert_eq!(
        saved.log_display.jstack_thread_filter_rules,
        app.config.log_display.jstack_thread_filter_rules
    );
    assert!(saved.log_display.jstack_thread_name_filters.is_empty());
    assert!(saved.log_display.jstack_stack_segment_filters.is_empty());
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
    app.active_tab_id = Some(2);
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

    app.apply_load_report(crate::loader::SourceTreeScanResult {
        registry,
        warnings: Vec::new(),
    });

    assert_eq!(visible_labels(&app), vec!["new.log"]);
    assert!(app.tabs.is_empty(), "替换来源后不应保留日志标签或占位标签");
    assert_eq!(app.active_tab_id, None);
    assert!(matches!(app.active_tab_kind(), TabKind::Empty));
    assert_eq!(app.next_tab_id, 3);
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
            kind: TabKind::RemoteFileManager { session_id: 9 },
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
    app.active_tab_id = Some(5);
    app.next_tab_id = 10;

    app.apply_load_report(crate::loader::SourceTreeScanResult {
        registry: placeholder_source_registry(),
        warnings: Vec::new(),
    });

    assert_eq!(app.tabs.len(), 2, "替换来源后只保留连接页签");
    assert!(
        app.tabs.iter().any(|tab| {
            tab.id == 4 && matches!(tab.kind, TabKind::SshTerminal { session_id: 7 })
        })
    );
    assert!(app.tabs.iter().any(|tab| {
        tab.id == 5 && matches!(tab.kind, TabKind::RemoteFileManager { session_id: 9 })
    }));
    assert!(!app.tabs.iter().any(|tab| tab.id == 8));
    assert!(matches!(
        app.active_tab_kind(),
        TabKind::RemoteFileManager { session_id: 9 }
    ));
    assert_eq!(app.active_tab_id, Some(5), "应保留原激活的连接页签");
    assert_eq!(app.next_tab_id, 10, "不再为占位标签分配 ID");
    assert!(app.terminal_sessions.contains_key(&7));
    assert!(app.remote_file_sessions.contains_key(&9));

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
    assert_eq!(app.active_tab_id, Some(10));
    assert_eq!(app.next_tab_id, 11);
    assert!(app.terminal_sessions.contains_key(&7));
    assert!(app.remote_file_sessions.contains_key(&9));
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
    let app_tab_id = app.active_tab_id.expect("应有激活标签");
    app.select_source(error_log_id);
    app.open_source_tree_search();
    app.update_source_tree_search_query("error".to_string());

    assert_eq!(visible_labels(&app), vec!["logs", "error.log"]);

    app.activate_tab(app_tab_id);

    assert_eq!(app.active_tab_id, Some(app_tab_id));
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
    let tab_id = push_test_log_tab(&mut app, "app.log");
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
    let tab_id = push_test_log_tab(&mut app, "app.log");

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

/// 构造 AES 加密测试压缩包；与 zip 适配器读取路径保持同一加密特性。
fn write_encrypted_zip(path: &Path, password: &str, entries: &[(&str, &[u8])]) {
    let mut writer =
        zip::ZipWriter::new(std::fs::File::create(path).expect("应创建加密测试压缩包"));
    for (name, content) in entries {
        writer
            .start_file(
                *name,
                zip::write::SimpleFileOptions::default()
                    .with_aes_encryption(zip::AesMode::Aes256, password),
            )
            .expect("应创建加密条目");
        std::io::Write::write_all(&mut writer, content).expect("应写入加密条目");
    }
    writer.finish().expect("应完成加密压缩包");
}

/// 完整加载指定加密压缩包路径，返回密码占位节点的来源 ID。
///
/// 复刻真实加载流程的同步版：物化（加密包转密码占位）→ 占位节点入树 → 工作目录入状态。
fn load_encrypted_archive_node(app: &mut ArgusApp, archive_path: &Path) -> SourceId {
    let workspace = crate::loader::workspace::materialize_sources(
        &[archive_path.to_path_buf()],
        &app.config.loader,
        &app.archive_passwords,
        &tokio_util::sync::CancellationToken::new(),
        None,
    )
    .expect("加密压缩包物化应转为密码占位");
    assert_eq!(workspace.password_pending.len(), 1);
    let mut registry = SourceRegistry::new();
    for archive in &workspace.password_pending {
        let id = registry.allocate_id();
        registry.insert_node(SourceTreeNode {
            id,
            parent_id: None,
            depth: 0,
            label: archive.label.clone(),
            kind: SourceKind::ArchivePasswordRequired,
            location: SourceLocation::LocalPath(archive.archive_path.clone()),
            metadata: SourceMetadata {
                children_loaded: true,
                message: Some("压缩包已加密，选择后输入密码展开".to_string()),
                ..SourceMetadata::default()
            },
            selected: false,
            expanded: false,
        });
    }
    registry.rebuild_all_indices();
    app.source_registry = registry;
    app.source_workspace_root = Some(workspace.root);
    let archive_id = app
        .source_registry
        .tree_order_source_ids()
        .iter()
        .copied()
        .find(|source_id| {
            app.source_registry
                .node(*source_id)
                .is_some_and(|node| node.label == "secret")
        })
        .expect("应存在加密压缩包占位节点");
    let node = app.source_registry.node(archive_id).unwrap();
    assert!(matches!(node.kind, SourceKind::ArchivePasswordRequired));
    archive_id
}

/// 验证新加载请求会取消在途完整加载任务，而不是被拒绝。
#[gpui::test]
fn new_source_load_cancels_in_flight_task(cx: &mut gpui::TestAppContext) {
    let directory = temporary_test_dir("source-load-cancel");
    std::fs::write(directory.path().join("app.log"), "ready").expect("应写入测试日志");
    let app = cx.new(|_| test_app());

    app.update(cx, |app, app_cx| {
        assert!(app.load_sources_from_paths(
            vec![directory.path().to_path_buf()],
            ExternalSourceTrigger::SourcePicker,
            app_cx,
        ));
    });
    let first_token = app.read_with(cx, |app, _| {
        assert!(app.is_source_loading);
        app.source_load_cancellation
            .clone()
            .expect("首个加载任务应持有取消令牌")
    });

    app.update(cx, |app, app_cx| {
        assert!(app.load_sources_from_paths(
            vec![directory.path().to_path_buf()],
            ExternalSourceTrigger::DragDrop,
            app_cx,
        ));
    });

    assert!(first_token.is_cancelled(), "在途任务必须被新加载请求取消");
    app.read_with(cx, |app, _| {
        assert!(app.is_source_loading, "新任务仍在进行，加载态应保持");
        assert_eq!(app.source_load_generation, 2);
        assert!(
            app.source_load_cancellation
                .as_ref()
                .is_some_and(|token| !token.is_cancelled())
        );
    });
}

/// 验证已被新加载取代的任务结果会被丢弃，不影响在途任务状态。
#[gpui::test]
fn stale_source_load_result_is_discarded(cx: &mut gpui::TestAppContext) {
    let app = cx.new(|_| test_app());

    app.update(cx, |app, app_cx| {
        app.is_source_loading = true;
        app.source_load_generation = 2;
        app.placeholder_notice = "正在完整加载".to_string();

        app.apply_source_load_result(1, Err(anyhow::anyhow!("来源树完整扫描已取消")), app_cx);
        assert!(app.is_source_loading, "过期取消结果不得归位加载态");
        assert_eq!(app.placeholder_notice, "正在完整加载");

        app.apply_source_load_result(2, Err(anyhow::anyhow!("磁盘读取失败")), app_cx);
        assert!(!app.is_source_loading);
        assert!(app.placeholder_notice.contains("磁盘读取失败"));
        assert!(app.source_load_cancellation.is_none());
    });
}

/// 验证已有日志时启动新加载会立即卸载旧日志工作区并进入加载状态，而不是继续展示过期内容。
#[gpui::test]
fn new_source_load_unloads_previous_workspace_immediately(cx: &mut gpui::TestAppContext) {
    let directory = temporary_test_dir("source-load-unload");
    std::fs::write(directory.path().join("new.log"), "ready").expect("应写入测试日志");
    let app = cx.new(|_| test_app());

    app.update(cx, |app, _| {
        app.source_registry = placeholder_source_registry();
        app.has_loaded_real_sources = true;
        app.tabs.push(ArgusTab {
            id: 2,
            title: "app.log".to_string(),
            kind: TabKind::LogSource {
                source_id: SourceId(2),
                path: "logs/app.log".to_string(),
            },
        });
        app.active_tab_id = Some(2);
        app.next_tab_id = 3;
    });

    app.update(cx, |app, app_cx| {
        assert!(app.load_sources_from_paths(
            vec![directory.path().to_path_buf()],
            ExternalSourceTrigger::SourcePicker,
            app_cx,
        ));
    });

    app.read_with(cx, |app, _| {
        assert!(app.is_source_loading);
        assert!(app.source_registry.is_empty(), "旧来源树应立即卸载");
        assert!(!app.has_loaded_real_sources);
        assert!(
            app.source_load_progress.is_some(),
            "应进入带进度展示的加载状态"
        );
        assert!(
            app.tabs.is_empty(),
            "旧日志标签应立即关闭，且不保留占位标签"
        );
        assert!(matches!(app.active_tab_kind(), TabKind::Empty));
    });
}

/// 验证加载结果无论成功失败，回填后都清理进度快照，内容区不再展示过期进度。
#[gpui::test]
fn applying_source_load_result_clears_progress(cx: &mut gpui::TestAppContext) {
    let app = cx.new(|_| test_app());

    app.update(cx, |app, app_cx| {
        app.is_source_loading = true;
        app.source_load_generation = 1;
        app.source_load_progress = Some(crate::loader::SourceTreeScanProgress {
            phase: crate::loader::SourceLoadPhase::Scanning,
            scanned: 3,
            current: "logs".to_string(),
        });
        app.apply_source_load_result(1, Err(anyhow::anyhow!("磁盘读取失败")), app_cx);
        assert!(!app.is_source_loading);
        assert!(app.source_load_progress.is_none(), "失败回填应清理进度");

        app.is_source_loading = true;
        app.source_load_generation = 2;
        app.source_load_progress = Some(crate::loader::SourceTreeScanProgress {
            phase: crate::loader::SourceLoadPhase::Scanning,
            scanned: 5,
            current: "logs/app.log".to_string(),
        });
        app.apply_source_load_result(
            2,
            Ok((
                crate::loader::MaterializedWorkspace {
                    root: std::path::PathBuf::from("/tmp/argus-test-workspace"),
                    roots: Vec::new(),
                    password_pending: Vec::new(),
                    warnings: Vec::new(),
                    materialized_files: 0,
                },
                crate::loader::SourceTreeScanResult {
                    registry: placeholder_source_registry(),
                    warnings: Vec::new(),
                },
            )),
            app_cx,
        );
        assert!(!app.is_source_loading);
        assert!(app.source_load_progress.is_none(), "成功回填应清理进度");
    });
}

/// 验证加载日志后单链目录（每级只有一个子目录）默认逐级展开，遇到分支即停止。
#[test]
fn source_load_expands_single_child_directory_chain() {
    let mut app = test_app();
    let mut registry = SourceRegistry::new();
    let push_directory =
        |registry: &mut SourceRegistry, parent_id: Option<SourceId>, depth: usize, label: &str| {
            let id = registry.allocate_id();
            registry.insert_node(crate::loader::SourceTreeNode {
                id,
                parent_id,
                depth,
                label: label.to_string(),
                kind: SourceKind::Directory,
                location: SourceLocation::LocalPath(PathBuf::from(label)),
                metadata: SourceMetadata {
                    children_loaded: true,
                    ..SourceMetadata::default()
                },
                selected: false,
                expanded: false,
            });
            id
        };
    let push_log =
        |registry: &mut SourceRegistry, parent_id: SourceId, depth: usize, label: &str| {
            let id = registry.allocate_id();
            registry.insert_node(crate::loader::SourceTreeNode {
                id,
                parent_id: Some(parent_id),
                depth,
                label: label.to_string(),
                kind: SourceKind::LogFile,
                location: SourceLocation::LocalPath(PathBuf::from(label)),
                metadata: SourceMetadata::default(),
                selected: false,
                expanded: false,
            });
            id
        };

    let logs_id = push_directory(&mut registry, None, 0, "logs");
    let month_id = push_directory(&mut registry, Some(logs_id), 1, "2026-10");
    let server_id = push_directory(&mut registry, Some(month_id), 2, "server");
    push_log(&mut registry, server_id, 3, "app.log");
    push_log(&mut registry, server_id, 3, "error.log");
    registry.rebuild_all_indices();

    assert!(app.apply_load_report(crate::loader::SourceTreeScanResult {
        registry,
        warnings: Vec::new(),
    }));

    assert_eq!(
        visible_labels(&app),
        vec!["logs", "2026-10", "server", "app.log", "error.log"],
        "单链目录应逐级展开到日志所在层"
    );
    assert!(
        app.source_registry
            .node(server_id)
            .is_some_and(|node| node.expanded),
        "含多个日志的末级目录也应展开"
    );
}

/// 验证来源加载后的侧栏默认状态：单文件来源自动收起目录树并直接打开文件，
/// 目录或多文件来源自动展开目录树（含上一次单文件加载收起后的重新加载）。
#[gpui::test]
fn source_load_panel_defaults_follow_source_shape(cx: &mut gpui::TestAppContext) {
    let app = cx.new(|_| test_app());

    app.update(cx, |app, app_cx| {
        let mut single_file_registry = crate::loader::SourceRegistry::new();
        let file_id = single_file_registry.allocate_id();
        single_file_registry.insert_node(crate::loader::SourceTreeNode {
            id: file_id,
            parent_id: None,
            depth: 0,
            label: "app.log".into(),
            kind: SourceKind::LogFile,
            location: crate::loader::SourceLocation::LocalPath(PathBuf::from("app.log")),
            metadata: crate::loader::SourceMetadata::default(),
            selected: false,
            expanded: false,
        });
        single_file_registry.rebuild_all_indices();

        app.source_load_generation = 1;
        app.apply_source_load_result(
            1,
            Ok((
                crate::loader::MaterializedWorkspace {
                    root: PathBuf::from("/tmp/argus-test-single-file"),
                    roots: Vec::new(),
                    password_pending: Vec::new(),
                    warnings: Vec::new(),
                    materialized_files: 1,
                },
                crate::loader::SourceTreeScanResult {
                    registry: single_file_registry,
                    warnings: Vec::new(),
                },
            )),
            app_cx,
        );
        assert!(app.is_source_panel_collapsed, "单文件来源应自动收起目录树");
        assert!(
            matches!(
                app.active_tab().map(|tab| &tab.kind),
                Some(TabKind::LogSource { source_id, .. }) if *source_id == file_id
            ),
            "单文件来源应直接打开文件"
        );
        assert!(
            matches!(
                app.log_read_state(file_id),
                Some(LogOpenState::Loading { .. })
            ),
            "单文件来源应自动触发后台读取，不再停留在等待读取状态"
        );

        // 目录或多文件来源：不重置折叠状态，验证在“单文件已收起”的现状上重新加载会
        // 自动展开目录树。
        app.source_load_generation = 2;
        app.apply_source_load_result(
            2,
            Ok((
                crate::loader::MaterializedWorkspace {
                    root: PathBuf::from("/tmp/argus-test-dir-source"),
                    roots: Vec::new(),
                    password_pending: Vec::new(),
                    warnings: Vec::new(),
                    materialized_files: 2,
                },
                crate::loader::SourceTreeScanResult {
                    registry: placeholder_source_registry(),
                    warnings: Vec::new(),
                },
            )),
            app_cx,
        );
        assert!(!app.is_source_panel_collapsed, "多文件来源应自动展开目录树");
    });
}

/// 验证点击待密码解锁的压缩包节点不展开，而是弹出密码输入框并记录子树重试动作。
#[gpui::test]
fn toggling_password_required_archive_opens_password_prompt(cx: &mut gpui::TestAppContext) {
    let directory = temporary_test_dir("archive-prompt-on-click");
    let secret_path = directory.path().join("secret.zip");
    write_encrypted_zip(&secret_path, "s3cret", &[("hidden.log", b"hidden")]);
    let app = cx.new(|_| test_app());
    let archive_id = app.update(cx, |app, _| load_encrypted_archive_node(app, &secret_path));

    app.update(cx, |app, app_cx| {
        app.toggle_source_expanded(archive_id, app_cx);

        assert!(!app.source_registry.node(archive_id).unwrap().expanded);
        let prompt = app
            .archive_password_prompt
            .as_ref()
            .expect("应弹出密码输入框");
        assert_eq!(prompt.error.kind, ArchivePasswordErrorKind::Required);
        assert_eq!(
            prompt.error.key,
            Some(ArchivePasswordKey::root(secret_path.clone()))
        );
        assert!(matches!(
            prompt.retry_action,
            ArchivePasswordRetryAction::ReloadArchiveNode { source_id } if source_id == archive_id
        ));
    });
}

/// 验证输入正确密码后向工作目录追加物化，原子替换占位节点为解压后的目录子树。
#[gpui::test]
fn archive_node_password_retry_unlocks_subtree(cx: &mut gpui::TestAppContext) {
    let directory = temporary_test_dir("archive-retry-unlock");
    let secret_path = directory.path().join("secret.zip");
    write_encrypted_zip(
        &secret_path,
        "s3cret",
        &[("hidden/a.log", b"a"), ("hidden/b.log", b"b")],
    );
    let app = cx.new(|_| test_app());
    let archive_id = app.update(cx, |app, _| load_encrypted_archive_node(app, &secret_path));

    app.update(cx, |app, _| {
        app.archive_passwords.insert(
            ArchivePasswordKey::root(secret_path.clone()),
            "s3cret".to_string(),
        );
        let workspace_root = app.source_workspace_root.clone().expect("工作目录应存在");
        let node = app.source_registry.node(archive_id).unwrap().clone();
        let result = crate::loader::workspace::append_materialize_archive(
            &workspace_root,
            &node.label,
            &secret_path,
            &app.config.loader,
            &app.archive_passwords,
            &tokio_util::sync::CancellationToken::new(),
        )
        .and_then(|root_info| {
            SourceTreeScanner::scan_paths(
                vec![root_info.path],
                app.config.loader.clone(),
                tokio_util::sync::CancellationToken::new(),
                None,
            )
        });
        app.apply_archive_node_retry_result(archive_id, result);
    });

    app.read_with(cx, |app, _| {
        let node = app.source_registry.node(archive_id).expect("节点应保留");
        assert!(matches!(node.kind, SourceKind::Directory));
        assert!(node.metadata.children_loaded);
        assert!(!node.metadata.is_loading);
        assert!(node.expanded);
        let labels = app
            .source_registry
            .tree_order_source_ids()
            .iter()
            .filter_map(|source_id| app.source_registry.node(*source_id))
            .map(|node| node.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(labels, vec!["secret", "hidden", "a.log", "b.log"]);
        assert!(app.placeholder_notice.contains("已解锁并展开"));
    });
}

/// 验证密码错误时会清除错误密码缓存并再次弹窗，节点保持可重试状态。
#[gpui::test]
fn archive_node_password_retry_invalid_password_reprompts(cx: &mut gpui::TestAppContext) {
    let directory = temporary_test_dir("archive-retry-invalid");
    let secret_path = directory.path().join("secret.zip");
    write_encrypted_zip(&secret_path, "s3cret", &[("hidden.log", b"hidden")]);
    let app = cx.new(|_| test_app());
    let archive_id = app.update(cx, |app, _| load_encrypted_archive_node(app, &secret_path));

    app.update(cx, |app, _| {
        let key = ArchivePasswordKey::root(secret_path.clone());
        app.archive_passwords
            .insert(key.clone(), "wrong".to_string());
        let workspace_root = app.source_workspace_root.clone().expect("工作目录应存在");
        let node = app.source_registry.node(archive_id).unwrap().clone();
        let result = crate::loader::workspace::append_materialize_archive(
            &workspace_root,
            &node.label,
            &secret_path,
            &app.config.loader,
            &app.archive_passwords,
            &tokio_util::sync::CancellationToken::new(),
        )
        .and_then(|root_info| {
            SourceTreeScanner::scan_paths(
                vec![root_info.path],
                app.config.loader.clone(),
                tokio_util::sync::CancellationToken::new(),
                None,
            )
        });
        app.apply_archive_node_retry_result(archive_id, result);

        let node = app.source_registry.node(archive_id).expect("节点应保留");
        assert!(!node.metadata.is_loading);
        assert!(matches!(node.kind, SourceKind::ArchivePasswordRequired));
        assert!(
            app.archive_passwords.get(&key).is_none(),
            "错误密码必须被清除，避免后续任务复用"
        );
        let prompt = app
            .archive_password_prompt
            .as_ref()
            .expect("密码错误后应再次弹窗");
        assert_eq!(prompt.error.kind, ArchivePasswordErrorKind::Invalid);
        assert!(matches!(
            prompt.retry_action,
            ArchivePasswordRetryAction::ReloadArchiveNode { source_id } if source_id == archive_id
        ));
    });
}

/// 验证各工作区与标签类型下内容玻璃板始终保留 8px 右侧窗口留白，不被窗口右缘裁掉。
///
/// 回归背景：终端栅格与 Runtime 表格按内容宽度参与布局时，玻璃板的最小宽度会超过窗口
/// 可用宽度，导致右侧留白消失、面板贴到窗口边缘。
#[gpui::test]
fn content_panel_keeps_right_inset_across_tab_kinds(cx: &mut gpui::TestAppContext) {
    use crate::ui::main_window::WINDOW_CONTENT_INSET;

    let (app, cx) = cx.add_window_view(|_, _| app_with_placeholder_sources());
    let link_id = app.update(cx, |app, _| add_test_ssh_link(app));
    app.update(cx, |app, _| {
        insert_test_terminal_session(app, 7, link_id);
        app.tabs = vec![ArgusTab {
            id: 4,
            title: "测试服务器".to_string(),
            kind: TabKind::SshTerminal { session_id: 7 },
        }];
        app.active_tab_id = Some(4);
        app.workspace = Workspace::Connections;
    });

    for width in [760.0_f32, 900.0, 1200.0, 1600.0] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(800.0)));
        let panel = cx
            .debug_bounds("window-content-panel")
            .expect("应渲染内容玻璃板");
        assert_eq!(
            f32::from(panel.right()),
            width - WINDOW_CONTENT_INSET,
            "终端标签在 {width} 宽度下应保留右侧窗口留白"
        );
    }

    app.update(cx, |app, _| {
        let analysis_id = runtime_analysis_ready_for_picker_test(app, "2026-06-25 14:25:03");
        app.tabs = vec![ArgusTab {
            id: 11,
            title: "Runtime分析".to_string(),
            kind: TabKind::RuntimeAnalysis { analysis_id },
        }];
        app.active_tab_id = Some(11);
    });
    for width in [760.0_f32, 1200.0] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(800.0)));
        let panel = cx
            .debug_bounds("window-content-panel")
            .expect("应渲染内容玻璃板");
        assert_eq!(
            f32::from(panel.right()),
            width - WINDOW_CONTENT_INSET,
            "Runtime 标签在 {width} 宽度下应保留右侧窗口留白"
        );
    }

    app.update(cx, |app, _| {
        let _receiver = insert_test_sftp_session(app, 9, link_id);
        app.tabs = vec![ArgusTab {
            id: 12,
            title: "SFTP".to_string(),
            kind: TabKind::RemoteFileManager { session_id: 9 },
        }];
        app.active_tab_id = Some(12);
    });
    for width in [760.0_f32, 1200.0] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(800.0)));
        let panel = cx
            .debug_bounds("window-content-panel")
            .expect("应渲染内容玻璃板");
        assert_eq!(
            f32::from(panel.right()),
            width - WINDOW_CONTENT_INSET,
            "远程文件管理标签在 {width} 宽度下应保留右侧窗口留白"
        );
    }
}

/// 验证来源侧栏宽度拖拽命中区与内容玻璃板左缘重合。
///
/// 回归背景：侧栏背景色与窗口背景色相同，侧栏与内容区之间唯一可见的分界线是
/// 内容玻璃板左侧边框；命中区曾以侧栏右缘为中心，比可见边框偏左 `WINDOW_CONTENT_INSET`，
/// 用户必须把鼠标移到边框左侧的空白带才能拖动。
#[gpui::test]
fn source_resizer_hit_area_follows_content_panel_left_edge(cx: &mut gpui::TestAppContext) {
    let (app, cx) = cx.add_window_view(|_, _| app_with_placeholder_sources());
    cx.simulate_resize(gpui::size(gpui::px(1400.0), gpui::px(900.0)));
    // 欢迎态默认收起侧栏；本用例验证展开状态下命中区几何（直接落到动画终点）。
    app.update(cx, |app, _| {
        app.is_source_panel_collapsed = false;
        let width = app.current_source_panel_width();
        app.source_panel_animation_from_width = width;
        app.source_panel_animation_to_width = width;
    });
    cx.simulate_resize(gpui::size(gpui::px(1401.0), gpui::px(900.0)));

    let assert_aligned = |cx: &mut gpui::VisualTestContext, scene: &str| {
        let hit_area = cx
            .debug_bounds("source-resizer-hit-area")
            .unwrap_or_else(|| panic!("{scene} 应渲染来源侧栏拖拽命中区"));
        let panel = cx
            .debug_bounds("window-content-panel")
            .unwrap_or_else(|| panic!("{scene} 应渲染内容玻璃板"));
        let hit_center = f32::from(hit_area.left()) + f32::from(hit_area.size.width) / 2.0;
        let panel_left = f32::from(panel.left());
        assert!(
            (hit_center - panel_left).abs() <= 0.5,
            "{scene} 拖拽命中区中心应与玻璃板左缘重合：命中区 {hit_center:.1} vs 玻璃板 {panel_left:.1}"
        );
        // 纵向只覆盖内容区：标题栏内没有这条边框，命中区不得与标签点击区抢事件。
        let window_height = f32::from(cx.update(|window, _| window.viewport_size().height));
        assert!(
            (f32::from(hit_area.top()) - crate::ui::custom_title_bar::TITLE_BAR_HEIGHT).abs()
                <= 0.5,
            "{scene} 拖拽命中区应从标题栏下方开始，实际 {:.1}px",
            f32::from(hit_area.top())
        );
        assert!(
            (f32::from(hit_area.bottom()) - window_height).abs() <= 0.5,
            "{scene} 拖拽命中区应延伸到窗口底部，实际 {:.1}px",
            f32::from(hit_area.bottom())
        );
        // 命中区本身透明、不占布局宽度，只以边框为中心向两侧各展开半个命中宽度。
        assert!(
            f32::from(hit_area.size.width) <= 8.0,
            "{scene} 拖拽命中区应保持窄条形状，实际 {:.1}px",
            f32::from(hit_area.size.width)
        );
    };

    assert_aligned(cx, "默认侧栏宽度");

    // 通过真实拖拽改变侧栏宽度后，命中区仍应贴合玻璃板左缘。
    app.update(cx, |app, _| {
        let start_x = app.current_source_panel_width();
        app.begin_source_panel_resize(start_x);
        assert!(
            app.resize_source_panel(start_x + 72.0),
            "拖拽 72px 应更新侧栏宽度"
        );
    });
    cx.simulate_resize(gpui::size(gpui::px(1401.0), gpui::px(900.0)));
    assert_aligned(cx, "调整侧栏宽度后");
}

/// 验证设置模态框的顶部留白结构：左侧背板与模态框同高，右侧玻璃板保留 8px 窗口留白。
///
/// 回归背景：浅色主题背板与玻璃板色差过小时，这里的 8px 留白在视觉上会消失，
/// 模态框顶部看起来贴边；本用例锁定布局关系，主题层另有色差下限用例。
#[gpui::test]
fn settings_modal_keeps_top_inset(cx: &mut gpui::TestAppContext) {
    use crate::ui::main_window::WINDOW_CONTENT_INSET;

    let (app, cx) = cx.add_window_view(|_, _| test_app());
    app.update(cx, |app, app_cx| app.open_settings_modal(app_cx));
    cx.simulate_resize(gpui::size(gpui::px(1400.0), gpui::px(900.0)));
    cx.simulate_resize(gpui::size(gpui::px(1401.0), gpui::px(900.0)));

    let sidebar = cx
        .debug_bounds("settings-modal-sidebar")
        .expect("应渲染设置侧栏");
    let content_area = cx
        .debug_bounds("settings-modal-content-area")
        .expect("应渲染设置内容区");
    let plate = cx
        .debug_bounds("settings-modal-plate")
        .expect("应渲染设置玻璃板");

    assert_eq!(
        f32::from(content_area.top()),
        f32::from(sidebar.top()),
        "内容区与侧栏应从模态框同一条顶边开始"
    );
    assert_eq!(
        f32::from(plate.top()) - f32::from(content_area.top()),
        WINDOW_CONTENT_INSET,
        "玻璃板顶部应保留 8px 窗口留白"
    );
}

/// 验证设置模态框顶部留白：侧栏标题与内容区标题同高，且都留有足够的顶部内边距。
///
/// 回归背景：侧栏标题曾距离模态框上边框只有约 19px，而内容区标题在 44px 处，
/// 两个标题不同线且左侧看起来贴住边框。
#[gpui::test]
fn settings_modal_titles_share_top_spacing(cx: &mut gpui::TestAppContext) {
    let (app, cx) = cx.add_window_view(|_, _| test_app());
    app.update(cx, |app, app_cx| app.open_settings_modal(app_cx));
    cx.simulate_resize(gpui::size(gpui::px(1400.0), gpui::px(900.0)));
    cx.simulate_resize(gpui::size(gpui::px(1401.0), gpui::px(900.0)));

    let sidebar = cx
        .debug_bounds("settings-modal-sidebar")
        .expect("应渲染设置侧栏");
    let sidebar_header = cx
        .debug_bounds("settings-modal-sidebar-header")
        .expect("应渲染设置侧栏标题头部");
    let sidebar_title = cx
        .debug_bounds("settings-modal-sidebar-title")
        .expect("应渲染设置侧栏标题");
    let section_label = cx
        .debug_bounds("settings-modal-section-label")
        .expect("应渲染内容区标题");

    assert_eq!(
        f32::from(sidebar_header.size.height),
        56.0,
        "侧栏标题头部应保持 56px 高度：分类较多时不得被弹性收缩压扁"
    );

    assert!(
        (f32::from(sidebar_title.top()) - f32::from(section_label.top())).abs() < 1.0,
        "两个标题应处于同一水平线：侧栏 {:.1} vs 内容区 {:.1}",
        f32::from(sidebar_title.top()),
        f32::from(section_label.top())
    );
    assert!(
        f32::from(sidebar_title.top()) - f32::from(sidebar.top()) >= 32.0,
        "侧栏标题上方应保留至少 32px 顶部留白，实际 {:.1}px",
        f32::from(sidebar_title.top()) - f32::from(sidebar.top())
    );
}

/// 验证 Runtime 四层下钻页的顶部栏不会挤掉下方明细表。
///
/// 回归背景：内容区包装容器曾使用 `min_w(0)`，使页签内容宽度进入固有尺寸计算；
/// 顶部栏内带 `min_w(0)` 的信息行因此被收窄到 min-content（约 63px），统计文案
/// 换行成多行把顶部栏撑到整屏高度，明细表被压到 6px，界面上只剩一个返回按钮。
#[gpui::test]
fn runtime_drilldown_topbars_keep_table_visible(cx: &mut gpui::TestAppContext) {
    let (app, cx) = cx.add_window_view(|_, _| app_with_placeholder_sources());
    cx.simulate_resize(gpui::size(gpui::px(1600.0), gpui::px(900.0)));
    let (analysis_id, request_path, normalized_sql) = app.update(cx, |app, _| {
        let analysis_id = runtime_analysis_ready_for_picker_test(app, "");
        let state = app
            .runtime_analysis_state(analysis_id)
            .expect("应存在 Runtime 分析状态");
        let RuntimeAnalysisTaskState::Ready(result) = &state.task_state else {
            panic!("Runtime 分析应处于就绪状态");
        };
        let summary = result.summaries.first().expect("测试数据应产出总览行");
        let normalized_sql = summary
            .request_indices
            .first()
            .and_then(|index| result.requests.get(*index))
            .and_then(|request| request.sql_records.first())
            .map(|sql| sql.normalized_sql.clone())
            .expect("测试数据应产出 SQL 记录");
        (analysis_id, summary.request_path.clone(), normalized_sql)
    });

    // 请求详情页：信息区必须占满剩余宽度，明细表必须保留可用高度。
    app.update(cx, |app, _| {
        app.open_runtime_request_details(analysis_id, request_path.clone());
    });
    next_frame(cx);
    assert_details_layout_is_usable(
        cx,
        "请求详情",
        "runtime-request-details-topbar",
        "runtime-request-details-body",
        Some("runtime-request-details-info"),
    );

    // SQL 列表页。
    app.update(cx, |app, _| {
        app.open_runtime_sql_list(analysis_id, request_path, 0);
    });
    next_frame(cx);
    assert_details_layout_is_usable(
        cx,
        "SQL 列表",
        "runtime-sql-details-topbar",
        "runtime-sql-details-body",
        None,
    );

    // SQL 频率分析下钻页。
    app.update(cx, |app, _| {
        app.open_runtime_sql_frequency_detail(analysis_id, normalized_sql.clone());
    });
    next_frame(cx);
    assert_details_layout_is_usable(
        cx,
        "SQL 频率分析明细",
        "runtime-sql-frequency-detail-topbar",
        "runtime-sql-frequency-detail-body",
        None,
    );

    // 慢 SQL 分析下钻页。
    app.update(cx, |app, _| {
        app.open_runtime_slow_sql_detail(analysis_id, normalized_sql);
    });
    next_frame(cx);
    assert_details_layout_is_usable(
        cx,
        "慢 SQL 分析明细",
        "runtime-slow-sql-detail-topbar",
        "runtime-slow-sql-detail-body",
        None,
    );

    // 窄窗口下顶部栏同样必须保持单行，不能靠换行撑高。
    cx.simulate_resize(gpui::size(gpui::px(900.0), gpui::px(700.0)));
    cx.simulate_resize(gpui::size(gpui::px(901.0), gpui::px(700.0)));
    assert_details_layout_is_usable(
        cx,
        "慢 SQL 分析明细（窄窗口）",
        "runtime-slow-sql-detail-topbar",
        "runtime-slow-sql-detail-body",
        None,
    );
}

/// 铺下一帧并保证尺寸与上一帧不同，使布局改动进入最新渲染帧。
fn next_frame(cx: &mut gpui::VisualTestContext) {
    let size = cx.update(|window, _| window.viewport_size());
    let alternate = if f32::from(size.width) == 1600.0 {
        1601.0
    } else {
        1600.0
    };
    cx.simulate_resize(gpui::size(gpui::px(alternate), size.height));
}

/// 断言下钻页顶部栏高度受限、明细表区域未被挤压，信息区宽度未被收窄到 min-content。
fn assert_details_layout_is_usable(
    cx: &mut gpui::VisualTestContext,
    scene: &str,
    topbar_selector: &'static str,
    body_selector: &'static str,
    info_selector: Option<&'static str>,
) {
    let topbar = cx
        .debug_bounds(topbar_selector)
        .unwrap_or_else(|| panic!("{scene} 应渲染顶部栏"));
    let body = cx
        .debug_bounds(body_selector)
        .unwrap_or_else(|| panic!("{scene} 应渲染明细表区域"));
    assert!(
        topbar.size.height < gpui::px(80.0),
        "{scene} 顶部栏应保持单行高度，实际 {:.1}px",
        f32::from(topbar.size.height)
    );
    assert!(
        body.size.height >= gpui::px(300.0),
        "{scene} 明细表区域不应被顶部栏挤压，实际 {:.1}px",
        f32::from(body.size.height)
    );
    if let Some(info_selector) = info_selector {
        let info = cx
            .debug_bounds(info_selector)
            .unwrap_or_else(|| panic!("{scene} 应渲染顶部栏信息区"));
        assert!(
            info.size.width >= gpui::px(300.0),
            "{scene} 信息区应占满剩余宽度，实际 {:.1}px",
            f32::from(info.size.width)
        );
    }
}
