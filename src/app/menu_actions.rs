//! 文件职责：提取右键上下文菜单和下拉菜单的打开、条目构建与动作分发方法到独立子模块。

use super::*;

impl ArgusApp {
    /// 在指定窗口坐标打开标签页右键菜单。
    pub fn open_tab_context_menu(&mut self, tab_id: usize, anchor: Point<Pixels>) {
        if !self.tabs.iter().any(|tab| tab.id == tab_id) {
            self.placeholder_notice = "未找到可操作的标签页".to_string();
            return;
        }

        self.tab_menu_scroll = UniformListScrollHandle::new();
        self.active_menu = Some(ActiveMenu {
            kind: ActiveMenuKind::TabContext { tab_id },
            anchor,
        });
    }

    /// 在指定窗口坐标打开全部标签页溢出菜单。
    pub fn open_tab_overflow_menu(&mut self, anchor: Point<Pixels>) {
        self.tab_menu_scroll = UniformListScrollHandle::new();
        self.active_menu = Some(ActiveMenu {
            kind: ActiveMenuKind::TabOverflow,
            anchor,
        });
    }

    /// 在搜索结果面板指定窗口坐标打开批量操作右键菜单。
    pub fn open_search_results_context_menu(&mut self, anchor: Point<Pixels>) {
        if self.log_search.result_groups.is_empty() {
            self.placeholder_notice = "暂无可操作的搜索结果分组".to_string();
            return;
        }

        self.tab_menu_scroll = UniformListScrollHandle::new();
        self.active_menu = Some(ActiveMenu {
            kind: ActiveMenuKind::SearchResultsPanel,
            anchor,
        });
    }

    /// 在来源树指定窗口坐标打开日志候选或 Runtime 目录节点右键菜单。
    pub fn open_source_tree_context_menu(&mut self, source_id: SourceId, anchor: Point<Pixels>) {
        let Some(source) = self.source_registry.node(source_id) else {
            self.placeholder_notice = "未找到可操作的来源节点".to_string();
            return;
        };
        if !self.source_supports_any_analysis_context_menu(source_id) {
            self.placeholder_notice = format!("{} 不是可分析的日志候选", source.label);
            return;
        }

        self.tab_menu_scroll = UniformListScrollHandle::new();
        self.active_menu = Some(ActiveMenu {
            kind: ActiveMenuKind::SourceTree { source_id },
            anchor,
        });
    }

    /// 在链接树指定窗口坐标打开目录或 SSH 链接右键菜单。
    pub fn open_connection_tree_context_menu(
        &mut self,
        node_id: ConnectionNodeId,
        anchor: Point<Pixels>,
    ) {
        if !self.config.connections.is_directory(node_id)
            && !self.config.connections.is_link(node_id)
        {
            self.placeholder_notice = "未找到可操作的连接节点".to_string();
            return;
        }

        self.selected_connection_node_id = Some(node_id);
        self.tab_menu_scroll = UniformListScrollHandle::new();
        self.active_menu = Some(ActiveMenu {
            kind: ActiveMenuKind::ConnectionTree { node_id },
            anchor,
        });
    }

    /// 在指定窗口坐标打开新增链接类型下拉菜单。
    pub fn open_connection_link_create_menu(&mut self, anchor: Point<Pixels>) {
        self.tab_menu_scroll = UniformListScrollHandle::new();
        self.active_menu = Some(ActiveMenu {
            kind: ActiveMenuKind::ConnectionLinkCreate,
            anchor,
        });
    }

    /// 关闭当前活动菜单。
    pub fn close_active_menu(&mut self) {
        self.active_menu = None;
    }

    /// 返回当前活动菜单应展示的菜单项。
    pub fn active_menu_entries(&self) -> Vec<MenuEntry> {
        let Some(active_menu) = &self.active_menu else {
            return Vec::new();
        };

        match active_menu.kind {
            ActiveMenuKind::TabContext { tab_id } => vec![
                MenuEntry::new("关闭当前", MenuAction::CloseTab { tab_id }),
                MenuEntry::new("关闭其他", MenuAction::CloseOtherTabs { tab_id }),
                MenuEntry::new("关闭全部", MenuAction::CloseAllTabs).danger(),
            ],
            ActiveMenuKind::TabOverflow => self
                .tabs
                .iter()
                .map(|tab| {
                    MenuEntry::new(
                        tab.title.clone(),
                        MenuAction::ActivateTab { tab_id: tab.id },
                    )
                    .selected(tab.id == self.active_tab_id)
                })
                .collect(),
            ActiveMenuKind::SearchResultsPanel => vec![
                MenuEntry::new("全部展开", MenuAction::ExpandAllSearchResults),
                MenuEntry::new("全部收起", MenuAction::CollapseAllSearchResults),
            ],
            ActiveMenuKind::SourceTree { source_id } => {
                let mut entries = Vec::new();
                if self.source_supports_jstack_analysis(source_id) {
                    entries.push(MenuEntry::new(
                        "Jstack线程日志分析",
                        MenuAction::OpenJstackAnalysis { source_id },
                    ));
                }
                if self.source_supports_runtime_analysis(source_id) {
                    entries.push(MenuEntry::new(
                        "Runtime日志解析",
                        MenuAction::OpenRuntimeAnalysis { source_id },
                    ));
                }
                entries
            }
            ActiveMenuKind::ConnectionTree { node_id } => {
                let (edit_label, delete_label) = if self.config.connections.is_directory(node_id) {
                    ("编辑目录", "删除目录")
                } else {
                    ("编辑链接", "删除链接")
                };
                vec![
                    MenuEntry::new(edit_label, MenuAction::EditConnectionNode { node_id }),
                    MenuEntry::new(delete_label, MenuAction::DeleteConnectionNode { node_id })
                        .danger(),
                ]
            }
            ActiveMenuKind::ConnectionLinkCreate => vec![
                MenuEntry::new("新建 SSH 链接", MenuAction::NewSshConnectionLink),
                MenuEntry::new("新建 SMB 链接", MenuAction::NewSmbConnectionLink),
            ],
            ActiveMenuKind::TerminalContext { session_id } => vec![MenuEntry::new(
                "文件管理",
                MenuAction::OpenSftpFileManager {
                    terminal_session_id: session_id,
                },
            )],
            ActiveMenuKind::SftpEntry { session_id } => {
                let mut entries = Vec::new();
                if self.can_preview_sftp_selection(session_id) {
                    entries.push(MenuEntry::new(
                        "预览",
                        MenuAction::PreviewSftpSelection { session_id },
                    ));
                }
                entries.push(MenuEntry::new(
                    "下载",
                    MenuAction::DownloadSftpSelection { session_id },
                ));
                entries.push(MenuEntry::new(
                    "重命名",
                    MenuAction::RenameSftpSelection { session_id },
                ));
                entries.push(
                    MenuEntry::new("删除", MenuAction::DeleteSftpSelection { session_id }).danger(),
                );
                entries
            }
        }
    }

    /// 执行通用菜单动作，并在动作完成后关闭菜单。
    pub fn handle_menu_action(&mut self, action: MenuAction) {
        match action {
            MenuAction::ActivateTab { tab_id } => self.activate_tab(tab_id),
            MenuAction::CloseTab { tab_id } => self.close_tab(tab_id),
            MenuAction::CloseOtherTabs { tab_id } => self.close_other_tabs(tab_id),
            MenuAction::CloseAllTabs => self.close_all_tabs(),
            MenuAction::ExpandAllSearchResults => self.expand_all_search_result_groups(),
            MenuAction::CollapseAllSearchResults => self.collapse_all_search_result_groups(),
            MenuAction::OpenJstackAnalysis { .. } => {
                self.placeholder_notice = "Jstack 分析需要从界面菜单触发".to_string();
            }
            MenuAction::OpenRuntimeAnalysis { .. } => {
                self.placeholder_notice = "Runtime 分析需要从界面菜单触发".to_string();
            }
            MenuAction::EditConnectionNode { .. } => {
                self.placeholder_notice = "连接编辑需要从界面菜单触发".to_string();
            }
            MenuAction::DeleteConnectionNode { node_id } => {
                self.request_delete_connection_node(node_id);
            }
            MenuAction::NewSshConnectionLink | MenuAction::NewSmbConnectionLink => {
                self.placeholder_notice = "新增链接需要从界面菜单触发".to_string();
            }
            MenuAction::OpenSftpFileManager { .. } => {
                self.placeholder_notice = "文件管理需要从界面菜单触发".to_string();
            }
            MenuAction::DownloadSftpSelection { .. } => {
                self.placeholder_notice = "文件下载需要从界面菜单触发".to_string();
            }
            MenuAction::PreviewSftpSelection { .. } => {
                self.placeholder_notice = "文件预览需要从界面菜单触发".to_string();
            }
            MenuAction::RenameSftpSelection { session_id } => {
                self.open_sftp_rename_dialog(session_id);
            }
            MenuAction::DeleteSftpSelection { session_id } => {
                self.request_delete_sftp_entry(session_id);
            }
        }

        self.close_active_menu();
    }

    /// 执行需要 GPUI 上下文的菜单动作；普通动作复用无上下文分发。
    pub fn handle_menu_action_with_context(&mut self, action: MenuAction, cx: &mut Context<Self>) {
        match action {
            MenuAction::ActivateTab { tab_id } => {
                self.activate_tab_with_context(tab_id, cx);
                self.close_active_menu();
            }
            MenuAction::CloseTab { tab_id } => {
                self.close_tab_with_context(tab_id, cx);
                self.close_active_menu();
            }
            MenuAction::CloseOtherTabs { tab_id } => {
                self.close_other_tabs_with_context(tab_id, cx);
                self.close_active_menu();
            }
            MenuAction::CloseAllTabs => {
                self.close_all_tabs_with_context(cx);
                self.close_active_menu();
            }
            MenuAction::OpenJstackAnalysis { source_id } => {
                self.open_jstack_analysis_tab(source_id, cx);
                self.close_active_menu();
            }
            MenuAction::OpenRuntimeAnalysis { source_id } => {
                self.open_runtime_analysis_tab(source_id, cx);
                self.close_active_menu();
            }
            MenuAction::EditConnectionNode { node_id } => {
                self.open_edit_connection_node_window(node_id, cx);
                self.close_active_menu();
            }
            MenuAction::DeleteConnectionNode { node_id } => {
                self.request_delete_connection_node(node_id);
                self.close_active_menu();
            }
            MenuAction::NewSshConnectionLink => {
                self.open_new_ssh_link_dialog(cx);
                self.close_active_menu();
            }
            MenuAction::NewSmbConnectionLink => {
                self.open_new_smb_link_dialog(cx);
                self.close_active_menu();
            }
            MenuAction::OpenSftpFileManager {
                terminal_session_id,
            } => {
                self.open_sftp_file_manager_from_terminal(terminal_session_id, cx);
                self.close_active_menu();
            }
            MenuAction::DownloadSftpSelection { session_id } => {
                self.choose_sftp_download_target(session_id, cx);
                self.close_active_menu();
            }
            MenuAction::PreviewSftpSelection { session_id } => {
                self.preview_sftp_selection(session_id);
                self.close_active_menu();
            }
            MenuAction::RenameSftpSelection { session_id } => {
                self.open_sftp_rename_dialog(session_id);
                self.close_active_menu();
            }
            MenuAction::DeleteSftpSelection { session_id } => {
                self.request_delete_sftp_entry(session_id);
                self.close_active_menu();
            }
            other => self.handle_menu_action(other),
        }
    }

}
