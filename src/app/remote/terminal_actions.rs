//! 文件职责：实现 SSH 终端标签的创建、事件回收、主机指纹确认和输入转发。
//! 创建日期：2026-06-26
//! 修改日期：2026-06-26
//! 作者：Argus 开发团队
//! 主要功能：把链接树中的 SSH 链接接入右侧终端面板，并负责关闭标签时释放后台会话。

use std::borrow::Borrow;

use gpui::{ClipboardItem, Context, Keystroke};

use crate::app::{ArgusApp, ArgusTab, TabKind, Workspace};
use crate::remote::connection::ConnectionNodeId;
use crate::remote::terminal::{
    DEFAULT_TERMINAL_COLS, DEFAULT_TERMINAL_ROWS, PendingHostKey, TerminalCommand, TerminalEvent,
    TerminalGridPosition, TerminalSessionState, TerminalStatus, TerminalWorkerRequest,
    spawn_ssh_worker, terminal_input_bytes,
};

impl ArgusApp {
    /// 为指定 SSH 链接打开新的终端标签；同一链接允许同时打开多个独立会话。
    pub(crate) fn open_or_focus_ssh_terminal(
        &mut self,
        link_id: ConnectionNodeId,
        cx: &mut Context<Self>,
    ) {
        self.create_ssh_terminal_session(link_id, cx);
    }

    /// 处理终端面板按键，将可发送的按键转换为远程 shell 字节。
    pub(crate) fn handle_terminal_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        let TabKind::SshTerminal { session_id } = self.active_tab_kind() else {
            return;
        };
        if keystroke.modifiers.platform {
            match keystroke.key.as_str() {
                "c" => {
                    self.copy_terminal_selection(session_id, cx);
                    return;
                }
                "a" => {
                    self.select_all_terminal(session_id);
                    return;
                }
                _ => {}
            }
        }
        if keystroke.key == "escape" && self.clear_terminal_selection(session_id) {
            return;
        }
        let Some(bytes) = terminal_input_bytes(
            keystroke.key.as_str(),
            keystroke.key_char.as_deref(),
            keystroke.modifiers.control,
            keystroke.modifiers.platform,
        ) else {
            return;
        };
        self.send_terminal_input(session_id, bytes);
    }

    /// 向指定终端会话发送输入字节。
    pub(crate) fn send_terminal_input(&mut self, session_id: usize, bytes: Vec<u8>) {
        let Some(session) = self.terminal_sessions.get_mut(&session_id) else {
            self.placeholder_notice = "终端会话不存在".to_string();
            return;
        };
        if session.status != TerminalStatus::Connected {
            self.placeholder_notice = "SSH 尚未连接，暂不能输入命令".to_string();
            return;
        }
        session.clear_selection();
        session.reset_scrollback();
        if let Some(sender) = &session.command_sender {
            let _ = sender.send(TerminalCommand::Input(bytes));
        }
    }

    /// 从指定行列开始终端文本选择。
    pub(crate) fn begin_terminal_selection(
        &mut self,
        session_id: usize,
        position: TerminalGridPosition,
        click_count: usize,
    ) {
        if let Some(session) = self.terminal_sessions.get_mut(&session_id) {
            session.begin_selection_with_click_count(position, click_count);
        }
    }

    /// 拖拽更新终端文本选区。
    pub(crate) fn update_terminal_selection(
        &mut self,
        session_id: usize,
        position: TerminalGridPosition,
    ) -> bool {
        self.terminal_sessions
            .get_mut(&session_id)
            .is_some_and(|session| session.update_selection(position))
    }

    /// 结束终端文本选择。
    pub(crate) fn finish_terminal_selection(&mut self, session_id: usize) -> bool {
        self.terminal_sessions
            .get_mut(&session_id)
            .is_some_and(TerminalSessionState::finish_selection)
    }

    /// 返回指定终端是否正在拖拽文本选区。
    pub(crate) fn is_terminal_selection_drag_active(&self, session_id: usize) -> bool {
        self.terminal_sessions
            .get(&session_id)
            .is_some_and(TerminalSessionState::is_selection_drag_active)
    }

    /// 清除指定终端文本选区。
    pub(crate) fn clear_terminal_selection(&mut self, session_id: usize) -> bool {
        self.terminal_sessions
            .get_mut(&session_id)
            .is_some_and(TerminalSessionState::clear_selection)
    }

    /// 平台复制快捷键：把当前终端选区写入剪贴板。
    pub(crate) fn copy_terminal_selection(&mut self, session_id: usize, cx: &mut Context<Self>) {
        let Some(selected_text) = self
            .terminal_sessions
            .get(&session_id)
            .and_then(TerminalSessionState::selected_text)
        else {
            self.placeholder_notice = "终端没有可复制的选区".to_string();
            return;
        };

        let app_context: &gpui::App = (*cx).borrow();
        app_context.write_to_clipboard(ClipboardItem::new_string(selected_text));
        self.placeholder_notice = "已复制终端选区".to_string();
    }

    /// 平台全选快捷键：选择当前可见终端屏幕。
    pub(crate) fn select_all_terminal(&mut self, session_id: usize) {
        if let Some(session) = self.terminal_sessions.get_mut(&session_id)
            && session.select_visible_screen()
        {
            self.placeholder_notice = "已选中当前终端屏幕".to_string();
        }
    }

    /// 处理终端正文滚轮，正数查看历史输出，负数回到实时输出。
    pub(crate) fn scroll_terminal_scrollback(
        &mut self,
        session_id: usize,
        line_delta: f32,
    ) -> bool {
        let Some(session) = self.terminal_sessions.get_mut(&session_id) else {
            return false;
        };
        session.scroll_scrollback_by(line_delta)
    }

    /// 开始拖动终端滚动条。
    pub(crate) fn begin_terminal_scrollbar_drag(&mut self, session_id: usize, cursor_offset: f32) {
        if let Some(session) = self.terminal_sessions.get_mut(&session_id) {
            session.begin_scrollbar_drag(cursor_offset);
        }
    }

    /// 按鼠标位置更新终端滚动条拖拽状态，并同步 scrollback 偏移。
    pub(crate) fn drag_terminal_scrollbar(
        &mut self,
        session_id: usize,
        pointer_y: f32,
        viewport_top: f32,
        track_start: f32,
        track_length: f32,
        thumb_length: f32,
        max_scrollback_offset: usize,
    ) -> bool {
        let Some(session) = self.terminal_sessions.get_mut(&session_id) else {
            return false;
        };
        let Some(cursor_offset) = session.scrollbar_drag_cursor_offset() else {
            return false;
        };
        let movable = (track_length - thumb_length).max(1.0);
        let thumb_start =
            (pointer_y - viewport_top - cursor_offset).clamp(track_start, track_start + movable);
        let ratio = ((thumb_start - track_start) / movable).clamp(0.0, 1.0);
        let rows_from_top = (ratio * max_scrollback_offset as f32).round() as usize;
        let scrollback_offset = max_scrollback_offset.saturating_sub(rows_from_top);
        session.set_scrollback_offset(scrollback_offset)
    }

    /// 结束终端滚动条拖拽。
    pub(crate) fn finish_terminal_scrollbar_drag(&mut self, session_id: usize) -> bool {
        let Some(session) = self.terminal_sessions.get_mut(&session_id) else {
            return false;
        };
        session.finish_scrollbar_drag()
    }

    /// 根据终端面板实际可用尺寸同步本地 vt100 屏幕和远程 PTY 行列。
    pub(crate) fn resize_terminal_session(&mut self, session_id: usize, rows: u16, cols: u16) {
        let Some(session) = self.terminal_sessions.get_mut(&session_id) else {
            return;
        };
        session.resize(rows, cols);
    }

    /// 用户确认当前 SSH 主机指纹可信，并继续后台 worker。
    pub(crate) fn confirm_terminal_host_key(&mut self, session_id: usize) {
        let Some((pending, sender)) = self.terminal_sessions.get(&session_id).and_then(|session| {
            session
                .pending_host_key
                .clone()
                .map(|pending| (pending, session.command_sender.clone()))
        }) else {
            self.placeholder_notice = "终端会话不存在".to_string();
            return;
        };

        self.config
            .connections
            .trust_host_key(&pending.host, pending.port, &pending.fingerprint);
        self.persist_config_or_report();
        if let Some(sender) = sender {
            let _ = sender.send(TerminalCommand::TrustHostKey);
        }
        if let Some(session) = self.terminal_sessions.get_mut(&session_id) {
            session.pending_host_key = None;
            session.status = TerminalStatus::Connecting;
            session.message = Some("已信任主机指纹，继续建立 SSH 连接...".to_string());
        }
        self.connection_dialog = None;
        self.placeholder_notice = "已信任 SSH 主机指纹".to_string();
    }

    /// 用户拒绝当前 SSH 主机指纹。
    pub(crate) fn reject_terminal_host_key(&mut self, session_id: usize) {
        let Some(session) = self.terminal_sessions.get_mut(&session_id) else {
            return;
        };
        if let Some(sender) = &session.command_sender {
            let _ = sender.send(TerminalCommand::RejectHostKey);
        }
        session.pending_host_key = None;
        session.status = TerminalStatus::Failed;
        session.message = Some("已拒绝信任 SSH 主机指纹".to_string());
        self.connection_dialog = None;
        self.placeholder_notice = "已拒绝 SSH 主机指纹".to_string();
    }

    /// 断开并移除指定终端会话。
    pub(crate) fn disconnect_terminal_session(&mut self, session_id: usize) {
        if let Some(session) = self.terminal_sessions.remove(&session_id)
            && let Some(sender) = session.command_sender
        {
            let _ = sender.send(TerminalCommand::Disconnect);
        }
    }

    /// 断开所有 SSH 终端会话。
    pub(crate) fn disconnect_all_terminal_sessions(&mut self) {
        let session_ids = self.terminal_sessions.keys().copied().collect::<Vec<_>>();
        for session_id in session_ids {
            self.disconnect_terminal_session(session_id);
        }
    }

    /// 创建新的 SSH 终端会话并启动后台 worker。
    fn create_ssh_terminal_session(&mut self, link_id: ConnectionNodeId, cx: &mut Context<Self>) {
        let Some(link) = self.config.connections.link(link_id).cloned() else {
            self.placeholder_notice = "未找到 SSH 链接".to_string();
            return;
        };
        let Some(ssh) = link.ssh_config().cloned() else {
            self.placeholder_notice = "当前链接不是 SSH 链接".to_string();
            return;
        };
        let session_id = self.next_terminal_session_id;
        self.next_terminal_session_id += 1;
        let trusted_fingerprint = self
            .config
            .connections
            .trusted_fingerprint(&ssh.host, ssh.port)
            .map(ToString::to_string);
        let request = TerminalWorkerRequest {
            session_id,
            ssh,
            trusted_fingerprint,
            rows: DEFAULT_TERMINAL_ROWS,
            cols: DEFAULT_TERMINAL_COLS,
        };
        let (command_sender, event_receiver) = spawn_ssh_worker(request);
        let session = TerminalSessionState::connecting(session_id, &link, command_sender);
        self.terminal_sessions.insert(session_id, session);
        self.create_terminal_tab_for_session(session_id);
        self.workspace = Workspace::Connections;
        self.placeholder_notice = format!("正在连接 {}", link.address_label());

        cx.spawn(async move |view, cx| {
            while let Ok(event) = event_receiver.recv().await {
                let _ = view.update(cx, |app, cx| {
                    app.apply_terminal_event(event);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// 为已有终端会话创建或复用标签。
    fn create_terminal_tab_for_session(&mut self, session_id: usize) {
        let Some(session) = self.terminal_sessions.get(&session_id) else {
            return;
        };
        let tab_id = if self.tabs.len() == 1 && matches!(self.tabs[0].kind, TabKind::Empty) {
            self.tabs[0].id
        } else {
            let next_id = self.next_tab_id;
            self.next_tab_id += 1;
            self.tabs.push(ArgusTab {
                id: next_id,
                title: String::new(),
                kind: TabKind::Empty,
            });
            next_id
        };

        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            tab.title = session.title.clone();
            tab.kind = TabKind::SshTerminal { session_id };
        }
        self.active_tab_id = tab_id;
    }

    /// 应用 SSH worker 回传事件。
    fn apply_terminal_event(&mut self, event: TerminalEvent) {
        match event {
            TerminalEvent::HostKeyVerification {
                session_id,
                host,
                port,
                fingerprint,
            } => self.apply_terminal_host_key_event(session_id, host, port, fingerprint),
            TerminalEvent::Connected { session_id } => {
                if let Some(session) = self.terminal_sessions.get_mut(&session_id) {
                    session.status = TerminalStatus::Connected;
                    session.message = Some("SSH 已连接".to_string());
                }
                self.placeholder_notice = "SSH 已连接".to_string();
            }
            TerminalEvent::Output { session_id, bytes } => {
                if let Some(session) = self.terminal_sessions.get_mut(&session_id) {
                    session.process_output(&bytes);
                }
            }
            TerminalEvent::Disconnected {
                session_id,
                message,
            } => {
                if let Some(session) = self.terminal_sessions.get_mut(&session_id) {
                    session.status = TerminalStatus::Disconnected;
                    session.command_sender = None;
                    session.message = Some(message.clone());
                }
                self.placeholder_notice = message;
            }
            TerminalEvent::Failed {
                session_id,
                message,
            } => {
                if let Some(session) = self.terminal_sessions.get_mut(&session_id) {
                    session.status = TerminalStatus::Failed;
                    session.command_sender = None;
                    session.message = Some(message.clone());
                }
                self.placeholder_notice = message;
            }
        }
    }

    /// 应用未知主机指纹事件，并打开确认弹窗。
    fn apply_terminal_host_key_event(
        &mut self,
        session_id: usize,
        host: String,
        port: u16,
        fingerprint: String,
    ) {
        let Some(link_id) = self
            .terminal_sessions
            .get(&session_id)
            .map(|session| session.link_id)
        else {
            return;
        };
        let pending = PendingHostKey {
            host,
            port,
            fingerprint,
        };
        if let Some(session) = self.terminal_sessions.get_mut(&session_id) {
            session.status = TerminalStatus::AwaitingHostKey;
            session.pending_host_key = Some(pending.clone());
            session.message = Some("请确认 SSH 主机指纹".to_string());
        }
        self.open_host_key_prompt(session_id, link_id, pending);
        self.placeholder_notice = "请确认 SSH 主机指纹".to_string();
    }
}
