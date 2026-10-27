//! 文件职责：实现 SSH 终端标签的创建、事件回收、主机指纹确认和输入转发。
//! 创建日期：2026-06-26
//! 修改日期：2026-10-15
//! 作者：Argus 开发团队
//! 主要功能：把链接树中的 SSH 链接接入右侧终端面板，并负责关闭标签时释放后台会话。

use std::borrow::Borrow;

use gpui::{ClipboardItem, Context, Entity, Keystroke, Pixels, Point, Window, point, px};

use crate::app::{ArgusApp, ArgusTab, TabKind, Workspace};
use crate::infra::selection_autoscroll::{
    selection_autoscroll_intensity, selection_autoscroll_step_lines,
};
use crate::remote::connection::{ConnectionLinkConfig, ConnectionNodeId};
use crate::remote::terminal::{
    DEFAULT_TERMINAL_COLS, DEFAULT_TERMINAL_ROWS, PendingHostKey, TerminalCommand, TerminalEvent,
    TerminalGridPosition, TerminalSelectionAutoscroll, TerminalSessionState, TerminalStatus,
    TerminalWorkerRequest, spawn_ssh_worker,
};
use crate::remote::terminal_input::{
    TerminalKeyInput, TerminalKeyModifiers, TerminalMouseButton, TerminalMouseEvent,
    TerminalMouseEventKind, TerminalMouseMode, encode_focus, encode_key, encode_mouse,
    encode_paste, terminal_input_chunks,
};
use crate::ui::terminal_view::{terminal_metrics, terminal_position_from_pointer};

impl ArgusApp {
    /// 为指定 SSH 链接打开新的终端标签；同一链接允许同时打开多个独立会话。
    pub(crate) fn open_or_focus_ssh_terminal(
        &mut self,
        link_id: ConnectionNodeId,
        cx: &mut Context<Self>,
    ) {
        self.create_ssh_terminal_session(link_id, cx);
    }

    /// 处理终端面板按键：平台快捷键保持本地，其余按键按远端输入模式编码后发送。
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
                "v" => {
                    self.paste_terminal_clipboard(session_id, cx);
                    return;
                }
                _ => {}
            }
        }
        if keystroke.key == "escape" && self.clear_terminal_selection(session_id) {
            return;
        }
        let Some(mode) = self
            .terminal_sessions
            .get(&session_id)
            .map(TerminalSessionState::input_mode)
        else {
            return;
        };
        let input = TerminalKeyInput {
            key: keystroke.key.as_str(),
            key_char: keystroke.key_char.as_deref(),
            modifiers: TerminalKeyModifiers {
                shift: keystroke.modifiers.shift,
                alt: keystroke.modifiers.alt,
                control: keystroke.modifiers.control,
                platform: keystroke.modifiers.platform,
                function: keystroke.modifiers.function,
            },
            option_as_meta: self.config.terminal.option_as_meta,
        };
        if let Some(bytes) = encode_key(&input, mode) {
            self.send_terminal_input(session_id, bytes);
        }
    }

    /// 向指定终端会话发送输入字节；大负载按固定分块拆分，避免单条命令占用过大。
    ///
    /// 说明：键盘与粘贴属于用户主动输入，会清理本地选区并回到实时屏幕。
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
        Self::send_terminal_bytes(session, &bytes);
    }

    /// 向指定终端会话写入控制序列字节（鼠标、焦点事件等），不清理本地选区与滚动位置。
    fn send_terminal_control_bytes(&mut self, session_id: usize, bytes: Vec<u8>) {
        let Some(session) = self.terminal_sessions.get_mut(&session_id) else {
            return;
        };
        if session.status != TerminalStatus::Connected {
            return;
        }
        Self::send_terminal_bytes(session, &bytes);
    }

    /// 按分块大小把字节写入会话命令通道。
    fn send_terminal_bytes(session: &TerminalSessionState, bytes: &[u8]) {
        if let Some(sender) = &session.command_sender {
            for chunk in terminal_input_chunks(bytes) {
                let _ = sender.send(TerminalCommand::Input(chunk.to_vec()));
            }
        }
    }

    /// 判断终端会话是否把鼠标事件交给远端程序；按住 Shift 时保持本地选择与滚动。
    pub(crate) fn terminal_forwards_mouse(&self, session_id: usize, shift: bool) -> bool {
        !shift
            && self
                .terminal_sessions
                .get(&session_id)
                .is_some_and(|session| session.input_mode().mouse_mode != TerminalMouseMode::None)
    }

    /// 判断远端是否要求上报全部鼠标移动事件（1003 模式）。
    pub(crate) fn terminal_mouse_reports_any_motion(&self, session_id: usize) -> bool {
        self.terminal_sessions
            .get(&session_id)
            .is_some_and(|session| session.input_mode().mouse_mode == TerminalMouseMode::AnyMotion)
    }

    /// 把终端鼠标事件编码后上报远端；远端未开启鼠标上报时不做任何事。
    pub(crate) fn send_terminal_mouse_event(
        &mut self,
        session_id: usize,
        event: TerminalMouseEvent,
    ) {
        let Some(mode) = self
            .terminal_sessions
            .get(&session_id)
            .map(TerminalSessionState::input_mode)
        else {
            return;
        };
        if let Some(bytes) = encode_mouse(&event, mode) {
            self.send_terminal_control_bytes(session_id, bytes);
        }
    }

    /// 转发终端鼠标按下事件；远端未开启鼠标模式时不做任何事，并按会话记录待释放按钮。
    pub(crate) fn forward_terminal_mouse_press(
        &mut self,
        session_id: usize,
        button: TerminalMouseButton,
        modifiers: TerminalKeyModifiers,
        col: u16,
        row: u16,
    ) {
        let Some(mode) = self
            .terminal_sessions
            .get(&session_id)
            .map(TerminalSessionState::input_mode)
        else {
            return;
        };
        let event = TerminalMouseEvent {
            kind: TerminalMouseEventKind::Press,
            button: Some(button),
            modifiers,
            col,
            row,
        };
        let Some(bytes) = encode_mouse(&event, mode) else {
            return;
        };
        if let Some(session) = self.terminal_sessions.get_mut(&session_id) {
            session.forwarded_mouse_button = Some(button);
        }
        self.send_terminal_control_bytes(session_id, bytes);
    }

    /// 转发终端鼠标释放事件；只有该按钮的按下事件确实转发过时才回发释放。
    pub(crate) fn forward_terminal_mouse_release(
        &mut self,
        session_id: usize,
        button: TerminalMouseButton,
        modifiers: TerminalKeyModifiers,
        col: u16,
        row: u16,
    ) {
        let pending = self
            .terminal_sessions
            .get_mut(&session_id)
            .and_then(|session| session.forwarded_mouse_button.take());
        if pending != Some(button) {
            return;
        }
        let Some(mode) = self
            .terminal_sessions
            .get(&session_id)
            .map(TerminalSessionState::input_mode)
        else {
            return;
        };
        let event = TerminalMouseEvent {
            kind: TerminalMouseEventKind::Release,
            button: Some(button),
            modifiers,
            col,
            row,
        };
        if let Some(bytes) = encode_mouse(&event, mode) {
            self.send_terminal_control_bytes(session_id, bytes);
        }
    }

    /// 判断指定按钮的按下事件是否已转发给远端，用于区分本地选择与远端鼠标释放。
    pub(crate) fn terminal_has_pending_mouse_button(
        &self,
        session_id: usize,
        button: TerminalMouseButton,
    ) -> bool {
        self.terminal_sessions
            .get(&session_id)
            .is_some_and(|session| session.forwarded_mouse_button == Some(button))
    }

    /// 上报终端视图焦点变化；远端未开启 1004 焦点事件时不做任何事。
    ///
    /// 首次上报只记录状态：会话刚建立时初始未聚焦不应向远端发送「失焦」事件。
    pub(crate) fn report_terminal_focus(&mut self, session_id: usize, is_focused: bool) {
        let Some(session) = self.terminal_sessions.get(&session_id) else {
            return;
        };
        if session.reported_focus == Some(is_focused) {
            return;
        }
        let is_first_report = session.reported_focus.is_none();
        let mode = session.input_mode();
        if let Some(session) = self.terminal_sessions.get_mut(&session_id) {
            session.reported_focus = Some(is_focused);
        }
        if is_first_report && !is_focused {
            return;
        }
        if let Some(bytes) = encode_focus(is_focused, mode) {
            self.send_terminal_control_bytes(session_id, bytes);
        }
    }

    /// 记录终端单元格像素尺寸，供远端 XTWINOPS 尺寸查询应答使用。
    pub(crate) fn set_terminal_cell_size(&mut self, session_id: usize, height: f32, width: f32) {
        if let Some(session) = self.terminal_sessions.get_mut(&session_id) {
            session.set_cell_size(height, width);
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
        self.clear_terminal_selection_autoscroll(session_id);
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
        self.clear_terminal_selection_autoscroll(session_id);
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

    /// 返回指定终端是否有可复制的非空选区，用于右键菜单显隐复制入口。
    pub(crate) fn can_copy_terminal_selection(&self, session_id: usize) -> bool {
        self.terminal_sessions
            .get(&session_id)
            .is_some_and(|session| session.selected_text().is_some())
    }

    /// 把剪贴板文本粘贴到指定终端；换行统一归一化为回车，符合终端行提交语义。
    pub(crate) fn paste_terminal_clipboard(&mut self, session_id: usize, cx: &mut Context<Self>) {
        let app_context: &gpui::App = (*cx).borrow();
        let clipboard_text = app_context
            .read_from_clipboard()
            .and_then(|item| item.text());
        let Some(text) = clipboard_text.filter(|text| !text.is_empty()) else {
            self.placeholder_notice = "剪贴板为空，无法粘贴".to_string();
            return;
        };
        let bracketed = self
            .terminal_sessions
            .get(&session_id)
            .is_some_and(|session| session.input_mode().bracketed_paste);
        self.send_terminal_input(session_id, encode_paste(&text, bracketed));
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

    /// 记录终端拖拽选择过程中的指针位置；指针越过视口纵向边缘时启动逐帧自动滚动循环。
    ///
    /// 说明：正文元素的 `on_mouse_move` 在指针拖出视口后不再触发，终端视图通过窗口级
    /// 传感器持续调用本入口；返回值为本次是否新启动了自动滚动循环。
    pub(crate) fn track_terminal_selection_autoscroll_pointer(
        &mut self,
        session_id: usize,
        pointer: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.is_terminal_selection_drag_active(session_id) {
            self.clear_terminal_selection_autoscroll(session_id);
            return false;
        }
        self.terminal_selection_autoscroll = Some(TerminalSelectionAutoscroll {
            session_id,
            pointer,
        });
        if self.terminal_selection_autoscroll_loop_active
            || !self.pointer_in_terminal_selection_autoscroll_zone(session_id, pointer)
        {
            return false;
        }
        self.terminal_selection_autoscroll_loop_active = true;
        schedule_terminal_selection_autoscroll_frame(cx.entity(), window);
        true
    }

    /// 拖拽选择期间执行一帧自动滚动，并把选区扩展到指针钳制在视口内后对应的边缘行列。
    ///
    /// 返回值：拖拽仍在进行、指针仍越出视口边缘且尚未滚到边界时返回 `true`，继续调度下一帧。
    pub(crate) fn step_terminal_selection_autoscroll(&mut self, window: &mut Window) -> bool {
        let Some(autoscroll) = self.terminal_selection_autoscroll else {
            return false;
        };
        let session_id = autoscroll.session_id;
        let pointer = autoscroll.pointer;
        let is_active_drag = self.is_terminal_selection_drag_active(session_id)
            && matches!(
                self.active_tab_kind(),
                TabKind::SshTerminal {
                    session_id: active_session_id
                } if active_session_id == session_id
            );
        if !is_active_drag {
            self.terminal_selection_autoscroll = None;
            return false;
        }
        let Some(session) = self.terminal_sessions.get(&session_id) else {
            self.terminal_selection_autoscroll = None;
            return false;
        };
        let bounds = session.viewport_scroll.bounds();
        if bounds.size.width <= px(0.0) || bounds.size.height <= px(0.0) {
            self.terminal_selection_autoscroll = None;
            return false;
        }
        let vertical_intensity = selection_autoscroll_intensity(
            f32::from(pointer.y),
            f32::from(bounds.top()),
            f32::from(bounds.bottom()),
        );
        if vertical_intensity == 0.0 {
            return false;
        }
        if terminal_autoscroll_at_boundary(
            vertical_intensity,
            session.parser.screen().scrollback(),
            session.max_scrollback_offset(),
        ) {
            return false;
        }
        let line_delta = terminal_autoscroll_line_delta(vertical_intensity);
        self.scroll_terminal_scrollback(session_id, line_delta);

        // 滚动后把选区扩展到指针钳制在视口内对应的边缘行列；行列随滚动逐帧向不可见区域推进。
        let clamped_point = point(
            pointer.x.clamp(bounds.left(), bounds.right()),
            pointer.y.clamp(bounds.top(), bounds.bottom()),
        );
        let metrics = terminal_metrics(window);
        if let Some(position) =
            terminal_position_from_pointer(self, session_id, clamped_point, metrics)
        {
            self.update_terminal_selection(session_id, position);
        }
        true
    }

    /// 判断指针是否越过终端视口纵向边缘。
    fn pointer_in_terminal_selection_autoscroll_zone(
        &self,
        session_id: usize,
        pointer: Point<Pixels>,
    ) -> bool {
        let Some(session) = self.terminal_sessions.get(&session_id) else {
            return false;
        };
        let bounds = session.viewport_scroll.bounds();
        if bounds.size.width <= px(0.0) || bounds.size.height <= px(0.0) {
            return false;
        }
        selection_autoscroll_intensity(
            f32::from(pointer.y),
            f32::from(bounds.top()),
            f32::from(bounds.bottom()),
        ) != 0.0
    }

    /// 清理指定终端的拖拽选择自动滚动状态；结束选择、清空选区和断开会话时统一走该入口。
    fn clear_terminal_selection_autoscroll(&mut self, session_id: usize) {
        if self
            .terminal_selection_autoscroll
            .is_some_and(|autoscroll| autoscroll.session_id == session_id)
        {
            self.terminal_selection_autoscroll = None;
        }
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
        self.clear_terminal_selection_autoscroll(session_id);
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
        let session_id = self.next_terminal_session_id;
        self.next_terminal_session_id += 1;
        if !self.start_ssh_terminal_worker(session_id, &link, cx) {
            return;
        }
        self.create_terminal_tab_for_session(session_id);
        self.workspace = Workspace::Connections;
        self.placeholder_notice = format!("正在连接 {}", link.address_label());
    }

    /// 重连已断开的 SSH 终端会话；复用同一会话 ID 与标签，重建 worker 与终端画面。
    pub(crate) fn reconnect_terminal_session(&mut self, session_id: usize, cx: &mut Context<Self>) {
        let Some((status, link_id)) = self
            .terminal_sessions
            .get(&session_id)
            .map(|session| (session.status.clone(), session.link_id))
        else {
            self.placeholder_notice = "终端会话不存在".to_string();
            return;
        };
        if !matches!(
            status,
            TerminalStatus::Disconnected | TerminalStatus::Failed
        ) {
            return;
        }
        let Some(link) = self.config.connections.link(link_id).cloned() else {
            self.placeholder_notice = "未找到 SSH 链接，无法重连".to_string();
            return;
        };
        if !self.start_ssh_terminal_worker(session_id, &link, cx) {
            return;
        }
        self.placeholder_notice = format!("正在重连 {}", link.address_label());
    }

    /// 为新的或重连的终端会话启动 SSH worker，并以全新会话状态替换同 ID 的旧状态。
    ///
    /// 返回值：worker 是否成功启动；链接不再是 SSH 链接时返回 false 并如实提示。
    ///
    /// 说明：重连必须重建会话状态——旧终端画面与旧命令通道随断开一起失效，
    /// 继续复用会让已断开的画面看起来仍可输入。
    fn start_ssh_terminal_worker(
        &mut self,
        session_id: usize,
        link: &ConnectionLinkConfig,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(ssh) = link.ssh_config().cloned() else {
            self.placeholder_notice = "当前链接不是 SSH 链接".to_string();
            return false;
        };
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
        self.terminal_sessions.insert(
            session_id,
            TerminalSessionState::connecting(session_id, link, command_sender),
        );
        Self::spawn_terminal_event_pump(cx, event_receiver);
        true
    }

    /// 消费 SSH worker 事件并回写应用状态，直到事件通道关闭。
    fn spawn_terminal_event_pump(
        cx: &mut Context<Self>,
        event_receiver: async_channel::Receiver<TerminalEvent>,
    ) {
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

/// 把自动滚动强度换算成终端 scrollback 行增量：指针越过顶边向历史滚动，越过底边向实时滚动；
/// 步长曲线与 Zed 编辑器一致，近边缘近乎静止、远边缘快速推进。
fn terminal_autoscroll_line_delta(vertical_intensity: f32) -> f32 {
    if vertical_intensity == 0.0 {
        return 0.0;
    }
    -selection_autoscroll_step_lines(vertical_intensity)
}

/// 判断自动滚动是否已到历史或实时边界；到达边界后逐帧循环停止，选区不再继续扩展。
fn terminal_autoscroll_at_boundary(
    vertical_intensity: f32,
    current_offset: usize,
    max_offset: usize,
) -> bool {
    (vertical_intensity < 0.0 && current_offset >= max_offset)
        || (vertical_intensity > 0.0 && current_offset == 0)
}

/// 调度终端拖拽选择自动滚动的下一帧；拖拽结束、指针回到视口内或滚到边界时循环自动停止。
fn schedule_terminal_selection_autoscroll_frame(entity: Entity<ArgusApp>, window: &mut Window) {
    window.on_next_frame(move |window, cx| {
        let keep_running =
            entity.update(cx, |app, _| app.step_terminal_selection_autoscroll(window));
        if keep_running {
            cx.notify(entity.entity_id());
            schedule_terminal_selection_autoscroll_frame(entity, window);
        } else {
            entity.update(cx, |app, _| {
                app.terminal_selection_autoscroll_loop_active = false;
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证顶边强度产生向历史的正行增量，底边强度产生向实时的负行增量，步长遵循 Zed 曲线。
    #[test]
    fn terminal_autoscroll_line_delta_follows_pointer_edge() {
        assert!(terminal_autoscroll_line_delta(-10.0) > 0.0);
        assert!(terminal_autoscroll_line_delta(10.0) < 0.0);
        assert_eq!(terminal_autoscroll_line_delta(0.0), 0.0);
        // 越界 8px：8^1.2 / 100 ≈ 0.121 行，近边缘近乎静止保证精准选择。
        let delta = terminal_autoscroll_line_delta(-8.0);
        assert!((delta - 0.1213).abs() < 0.001);
    }

    /// 验证到达历史或实时边界时自动滚动循环应停止，未到时继续。
    #[test]
    fn terminal_autoscroll_boundary_stops_frame_loop() {
        assert!(terminal_autoscroll_at_boundary(-1.0, 5, 5));
        assert!(!terminal_autoscroll_at_boundary(-1.0, 4, 5));
        assert!(terminal_autoscroll_at_boundary(1.0, 0, 5));
        assert!(!terminal_autoscroll_at_boundary(1.0, 1, 5));
        assert!(!terminal_autoscroll_at_boundary(0.0, 0, 5));
    }
}
