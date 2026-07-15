//! 文件职责：封装 SSH 终端会话、主机指纹校验、终端输出解析和按键字节映射。
//! 创建日期：2026-06-26
//! 修改日期：2026-06-26
//! 作者：Argus 开发团队
//! 主要功能：使用内嵌 ssh2 通道建立远程 shell，并把后台输出安全回传给 GPUI 状态。

use std::io::{Read, Write};
use std::ops::Range;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{Context as AnyhowContext, Result, anyhow, bail};
use async_channel::{Receiver, Sender};
use gpui::ScrollHandle;

use crate::infra::text_selection::{
    TextSelectionGranularity, byte_index_for_character, char_column_for_byte_index,
    character_count, word_range_at,
};
use crate::remote::connection::{ConnectionLinkConfig, ConnectionNodeId, SshLinkConfig};
use crate::remote::ssh::{authenticate_ssh_session, connect_ssh_session};

/// 终端默认行数。
pub(crate) const DEFAULT_TERMINAL_ROWS: u16 = 30;
/// 终端默认列数。
pub(crate) const DEFAULT_TERMINAL_COLS: u16 = 100;
/// 终端滚屏缓存行数，避免短时间输出过多时丢失最近上下文。
const TERMINAL_SCROLLBACK_LINES: usize = 2000;
/// SSH 后台循环空闲等待间隔，平衡响应速度和 CPU 占用。
const SSH_WORKER_IDLE_SLEEP: Duration = Duration::from_millis(12);

/// SSH 终端运行期状态，存放在 `ArgusApp` 中并由 UI 渲染。
pub(crate) struct TerminalSessionState {
    /// 终端会话 ID，与标签页中的 `session_id` 对应。
    pub id: usize,
    /// 关联的链接节点 ID。
    pub link_id: ConnectionNodeId,
    /// 终端标签标题。
    pub title: String,
    /// 当前终端状态。
    pub status: TerminalStatus,
    /// 终端输出解析器，负责把 ANSI 控制序列还原成屏幕文本。
    pub parser: vt100::Parser,
    /// 发送给 SSH 后台线程的命令通道。
    pub command_sender: Option<mpsc::Sender<TerminalCommand>>,
    /// 等待用户确认的主机指纹；没有待确认时为空。
    pub pending_host_key: Option<PendingHostKey>,
    /// 当前远程 PTY 行数。
    pub rows: u16,
    /// 当前远程 PTY 列数。
    pub cols: u16,
    /// 当前可回看的最大 scrollback 偏移，用于 UI 计算滚动条比例。
    max_scrollback_offset: usize,
    /// 精确滚轮产生的未满一行滚动余量，避免触控板小步滚动完全失效。
    scrollback_line_remainder: f32,
    /// 终端滚动条拖拽时鼠标在滑块内的纵向偏移。
    scrollbar_drag_cursor_offset: Option<f32>,
    /// 终端正文视口句柄，用于元素化渲染时读取可用尺寸和鼠标局部坐标。
    pub viewport_scroll: ScrollHandle,
    /// 当前终端文本选区；行列坐标均基于当前可见屏幕。
    pub selection: Option<TerminalTextSelection>,
    /// 鼠标拖拽中的终端文本选区锚点与粒度；没有拖拽时为空。
    selection_drag: Option<TerminalSelectionDrag>,
    /// 最近一次失败或断开提示。
    pub message: Option<String>,
}

impl TerminalSessionState {
    /// 创建一个处于“连接中”的终端会话状态。
    pub(crate) fn connecting(
        id: usize,
        link: &ConnectionLinkConfig,
        command_sender: mpsc::Sender<TerminalCommand>,
    ) -> Self {
        Self {
            id,
            link_id: link.id,
            title: link.name.clone(),
            status: TerminalStatus::Connecting,
            parser: vt100::Parser::new(
                DEFAULT_TERMINAL_ROWS,
                DEFAULT_TERMINAL_COLS,
                TERMINAL_SCROLLBACK_LINES,
            ),
            command_sender: Some(command_sender),
            pending_host_key: None,
            rows: DEFAULT_TERMINAL_ROWS,
            cols: DEFAULT_TERMINAL_COLS,
            max_scrollback_offset: 0,
            scrollback_line_remainder: 0.0,
            scrollbar_drag_cursor_offset: None,
            viewport_scroll: ScrollHandle::new(),
            selection: None,
            selection_drag: None,
            message: Some("正在建立 SSH 连接...".to_string()),
        }
    }

    /// 将后台输出写入 vt100 解析器。
    pub(crate) fn process_output(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
        self.refresh_max_scrollback_offset();
    }

    /// 生成当前终端屏幕快照，供 UI 按终端单元格绘制颜色、背景和光标。
    pub(crate) fn screen_snapshot(&self) -> TerminalScreenSnapshot {
        let screen = self.parser.screen();
        let (cursor_row, cursor_col) = screen.cursor_position();
        let scrollback_offset = screen.scrollback();
        let lines = self.screen_lines();
        let mut runs = Vec::new();
        for row in 0..self.rows {
            let mut active_run: Option<TerminalCellRun> = None;
            let mut col = 0;
            while col < self.cols {
                let Some(cell) = screen.cell(row, col) else {
                    flush_terminal_run(&mut active_run, &mut runs);
                    col += 1;
                    continue;
                };
                if cell.is_wide_continuation() {
                    col += 1;
                    continue;
                }

                let style = TerminalCellStyle::from_cell(cell);
                let cell_cols = if cell.is_wide() { 2 } else { 1 };
                let text = if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                };
                let should_render =
                    cell.has_contents() || style.bg != TerminalColor::Default || style.is_inverse;
                if !should_render {
                    flush_terminal_run(&mut active_run, &mut runs);
                    col += cell_cols;
                    continue;
                }

                append_terminal_run(&mut active_run, &mut runs, row, col, cell_cols, text, style);
                col += cell_cols;
            }
            flush_terminal_run(&mut active_run, &mut runs);
        }

        TerminalScreenSnapshot {
            rows: self.rows,
            cols: self.cols,
            cursor_row,
            cursor_col,
            is_cursor_hidden: screen.hide_cursor() || scrollback_offset > 0,
            scrollback_offset,
            max_scrollback_offset: self.max_scrollback_offset,
            lines,
            selection: self.selection,
            runs,
        }
    }

    /// 返回可见屏幕的完整文本行，并保留终端列到 UTF-8 字节位置的映射。
    fn screen_lines(&self) -> Vec<TerminalScreenLine> {
        let screen = self.parser.screen();
        (0..self.rows)
            .map(|row| terminal_screen_line(screen, row, self.cols))
            .collect()
    }

    /// 根据滚轮行数调整 scrollback 偏移；正数查看更早输出，负数回到更新输出。
    pub(crate) fn scroll_scrollback_by(&mut self, line_delta: f32) -> bool {
        if line_delta == 0.0 || !line_delta.is_finite() {
            return false;
        }
        self.scrollback_line_remainder += line_delta;
        let whole_lines = if self.scrollback_line_remainder >= 1.0 {
            self.scrollback_line_remainder.floor() as isize
        } else if self.scrollback_line_remainder <= -1.0 {
            self.scrollback_line_remainder.ceil() as isize
        } else {
            return false;
        };
        self.scrollback_line_remainder -= whole_lines as f32;

        let current = self.parser.screen().scrollback();
        let next = if whole_lines > 0 {
            current.saturating_add(whole_lines as usize)
        } else {
            current.saturating_sub(whole_lines.unsigned_abs())
        };
        self.set_scrollback_offset(next)
    }

    /// 回到实时终端屏幕，通常在用户开始输入时调用。
    pub(crate) fn reset_scrollback(&mut self) -> bool {
        self.scrollback_line_remainder = 0.0;
        self.set_scrollback_offset(0)
    }

    /// 将 scrollback 偏移设置到指定行，超出范围时由 vt100 自动夹紧。
    pub(crate) fn set_scrollback_offset(&mut self, offset: usize) -> bool {
        let screen = self.parser.screen_mut();
        let current = screen.scrollback();
        screen.set_scrollback(offset);
        let changed = screen.scrollback() != current;
        self.refresh_max_scrollback_offset();
        changed
    }

    /// 开始拖动终端滚动条，保存鼠标在滑块内的偏移。
    pub(crate) fn begin_scrollbar_drag(&mut self, cursor_offset: f32) {
        self.scrollbar_drag_cursor_offset = Some(cursor_offset.max(0.0));
    }

    /// 返回当前滚动条拖拽偏移；没有拖拽时为空。
    pub(crate) fn scrollbar_drag_cursor_offset(&self) -> Option<f32> {
        self.scrollbar_drag_cursor_offset
    }

    /// 结束滚动条拖拽。
    pub(crate) fn finish_scrollbar_drag(&mut self) -> bool {
        let was_dragging = self.scrollbar_drag_cursor_offset.is_some();
        self.scrollbar_drag_cursor_offset = None;
        was_dragging
    }

    /// 从指定终端行列开始文本选择。
    #[cfg(test)]
    pub(crate) fn begin_selection(&mut self, position: TerminalGridPosition) {
        self.begin_selection_with_granularity(position, TextSelectionGranularity::Character);
    }

    /// 根据点击次数从指定终端行列开始文本选择。
    pub(crate) fn begin_selection_with_click_count(
        &mut self,
        position: TerminalGridPosition,
        click_count: usize,
    ) {
        self.begin_selection_with_granularity(
            position,
            terminal_selection_granularity_for_click_count(click_count),
        );
    }

    /// 从指定终端行列和选择粒度开始文本选择。
    pub(crate) fn begin_selection_with_granularity(
        &mut self,
        position: TerminalGridPosition,
        granularity: TextSelectionGranularity,
    ) {
        let position = self.clamp_grid_position(position);
        let selection = self.selection_for_position(position, granularity);
        self.selection = Some(selection);
        self.selection_drag = Some(TerminalSelectionDrag {
            anchor_range: selection,
            granularity,
        });
    }

    /// 拖拽过程中更新终端文本选择范围。
    pub(crate) fn update_selection(&mut self, position: TerminalGridPosition) -> bool {
        let Some(drag) = self.selection_drag.clone() else {
            return false;
        };
        let focus_range =
            self.selection_for_position(self.clamp_grid_position(position), drag.granularity);
        let next_selection = merge_terminal_text_selections(&drag.anchor_range, &focus_range);
        if self.selection == Some(next_selection) {
            return false;
        }
        self.selection = Some(next_selection);
        true
    }

    /// 结束终端文本选择；空选区会被清除。
    pub(crate) fn finish_selection(&mut self) -> bool {
        let was_dragging = self.selection_drag.is_some();
        self.selection_drag = None;
        if self.selection.is_some_and(|selection| selection.is_empty()) {
            self.selection = None;
        }
        was_dragging
    }

    /// 返回当前是否正在拖拽终端文本选区。
    pub(crate) fn is_selection_drag_active(&self) -> bool {
        self.selection_drag.is_some()
    }

    /// 清除终端文本选区。
    pub(crate) fn clear_selection(&mut self) -> bool {
        let changed = self.selection.is_some() || self.selection_drag.is_some();
        self.selection = None;
        self.selection_drag = None;
        changed
    }

    /// 选择当前可见终端屏幕，供平台全选快捷键使用。
    pub(crate) fn select_visible_screen(&mut self) -> bool {
        if self.rows == 0 || self.cols == 0 {
            return false;
        }
        self.selection = Some(TerminalTextSelection {
            anchor: TerminalGridPosition { row: 0, col: 0 },
            focus: TerminalGridPosition {
                row: self.rows.saturating_sub(1),
                col: self.cols,
            },
        });
        self.selection_drag = None;
        true
    }

    /// 返回当前终端选区文本；空选区或无选区时返回空。
    pub(crate) fn selected_text(&self) -> Option<String> {
        let selection = self.selection.filter(|selection| !selection.is_empty())?;
        let (start, end) = selection.normalized();
        let screen = self.parser.screen();
        let text = screen.contents_between(start.row, start.col, end.row, end.col);
        (!text.is_empty()).then_some(text)
    }

    /// 更新终端尺寸，并同步本地解析器尺寸。
    pub(crate) fn resize(&mut self, rows: u16, cols: u16) {
        if self.rows == rows && self.cols == cols {
            return;
        }
        self.rows = rows.max(1);
        self.cols = cols.max(1);
        self.clear_selection();
        self.parser.screen_mut().set_size(self.rows, self.cols);
        self.refresh_max_scrollback_offset();
        if let Some(sender) = &self.command_sender {
            let _ = sender.send(TerminalCommand::Resize {
                rows: self.rows,
                cols: self.cols,
            });
        }
    }

    /// 刷新最大历史偏移缓存；vt100 只提供设置后夹紧的方式获取该值。
    fn refresh_max_scrollback_offset(&mut self) {
        let screen = self.parser.screen_mut();
        let current = screen.scrollback();
        screen.set_scrollback(usize::MAX);
        let max_offset = screen.scrollback();
        screen.set_scrollback(current.min(max_offset));
        self.max_scrollback_offset = max_offset;
    }

    /// 把外部传入的行列坐标限制在当前屏幕范围内。
    fn clamp_grid_position(&self, position: TerminalGridPosition) -> TerminalGridPosition {
        TerminalGridPosition {
            row: position.row.min(self.rows.saturating_sub(1)),
            col: position.col.min(self.cols),
        }
    }

    /// 根据终端行列和选择粒度生成选区范围。
    fn selection_for_position(
        &self,
        position: TerminalGridPosition,
        granularity: TextSelectionGranularity,
    ) -> TerminalTextSelection {
        match granularity {
            TextSelectionGranularity::Character => TerminalTextSelection {
                anchor: position,
                focus: position,
            },
            TextSelectionGranularity::Word => self.word_selection_for_position(position),
            TextSelectionGranularity::Line => TerminalTextSelection {
                anchor: TerminalGridPosition {
                    row: position.row,
                    col: 0,
                },
                focus: TerminalGridPosition {
                    row: position.row,
                    col: self.cols,
                },
            },
        }
    }

    /// 生成当前行的单词选区；点击空白或标点时保留空选区。
    fn word_selection_for_position(&self, position: TerminalGridPosition) -> TerminalTextSelection {
        let Some(line) = self.screen_lines().get(usize::from(position.row)).cloned() else {
            return TerminalTextSelection {
                anchor: position,
                focus: position,
            };
        };
        let cursor_byte = line
            .byte_range_for_columns(position.col..position.col)
            .start;
        let cursor = char_column_for_byte_index(&line.text, cursor_byte);
        let line_char_count = character_count(&line.text);
        let range = word_range_at(&line.text, cursor.min(line_char_count))
            .unwrap_or(cursor.min(line_char_count)..cursor.min(line_char_count));
        let start_byte = byte_index_for_character(&line.text, range.start);
        let end_byte = byte_index_for_character(&line.text, range.end);

        TerminalTextSelection {
            anchor: TerminalGridPosition {
                row: position.row,
                col: line.column_for_byte_boundary(start_byte),
            },
            focus: TerminalGridPosition {
                row: position.row,
                col: line.column_for_byte_boundary(end_byte),
            },
        }
    }
}

/// 终端屏幕中的一个行列坐标，列允许等于 `cols` 以表示行尾边界。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct TerminalGridPosition {
    /// 0 基终端行号。
    pub row: u16,
    /// 0 基终端列号；作为选区终点时可等于终端列数。
    pub col: u16,
}

/// 终端文本选区，由锚点和当前焦点行列组成。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminalTextSelection {
    /// 选区开始拖拽时的锚点。
    pub anchor: TerminalGridPosition,
    /// 选区当前焦点。
    pub focus: TerminalGridPosition,
}

/// 鼠标拖拽中的终端选区上下文，用于双击拖拽时保持词级扩展。
#[derive(Clone, Debug, Eq, PartialEq)]
struct TerminalSelectionDrag {
    /// 开始拖拽时的选区范围。
    anchor_range: TerminalTextSelection,
    /// 当前拖拽的选择粒度。
    granularity: TextSelectionGranularity,
}

impl TerminalTextSelection {
    /// 返回从小到大排列后的选区端点。
    pub(crate) fn normalized(&self) -> (TerminalGridPosition, TerminalGridPosition) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    /// 判断选区是否没有覆盖任何终端单元格。
    pub(crate) fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }
}

/// 根据鼠标点击次数返回终端文本选择粒度。
fn terminal_selection_granularity_for_click_count(click_count: usize) -> TextSelectionGranularity {
    match click_count {
        0 | 1 => TextSelectionGranularity::Character,
        2 => TextSelectionGranularity::Word,
        _ => TextSelectionGranularity::Line,
    }
}

/// 合并两个终端选区范围，确保拖拽时完整覆盖起始词/行和当前词/行。
fn merge_terminal_text_selections(
    anchor_range: &TerminalTextSelection,
    focus_range: &TerminalTextSelection,
) -> TerminalTextSelection {
    let (anchor_start, anchor_end) = anchor_range.normalized();
    let (focus_start, focus_end) = focus_range.normalized();
    TerminalTextSelection {
        anchor: anchor_start.min(focus_start),
        focus: anchor_end.max(focus_end),
    }
}

/// 终端屏幕单行文本及列边界到 UTF-8 字节偏移的映射。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TerminalScreenLine {
    /// 按终端单元格补齐到当前列数的显示文本。
    pub text: String,
    /// 终端列边界到 `text` 字节下标的映射，长度为 `cols + 1`。
    column_byte_indices: Vec<usize>,
}

impl TerminalScreenLine {
    /// 返回终端列范围在当前行文本中的 UTF-8 字节范围。
    pub(crate) fn byte_range_for_columns(&self, range: Range<u16>) -> Range<usize> {
        let last_index = self.column_byte_indices.len().saturating_sub(1);
        let start_col = usize::from(range.start).min(last_index);
        let end_col = usize::from(range.end).min(last_index).max(start_col);
        let start = self
            .column_byte_indices
            .get(start_col)
            .copied()
            .unwrap_or(self.text.len())
            .min(self.text.len());
        let end = self
            .column_byte_indices
            .get(end_col)
            .copied()
            .unwrap_or(self.text.len())
            .min(self.text.len())
            .max(start);
        start..end
    }

    /// 返回指定 UTF-8 字节边界对应的终端列；宽字符会映射到占用列的末端边界。
    pub(crate) fn column_for_byte_boundary(&self, byte_index: usize) -> u16 {
        let byte_index = byte_index.min(self.text.len());
        self.column_byte_indices
            .iter()
            .rposition(|index| *index <= byte_index)
            .unwrap_or_default() as u16
    }
}

/// 终端屏幕快照，避免 UI 直接持有 vt100 屏幕借用。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TerminalScreenSnapshot {
    /// 当前终端可见行数。
    pub rows: u16,
    /// 当前终端可见列数。
    pub cols: u16,
    /// 光标所在行。
    pub cursor_row: u16,
    /// 光标所在列。
    pub cursor_col: u16,
    /// 远端应用是否要求隐藏光标。
    pub is_cursor_hidden: bool,
    /// 当前 scrollback 偏移，0 表示实时屏幕。
    pub scrollback_offset: usize,
    /// 可回看的最大 scrollback 偏移。
    pub max_scrollback_offset: usize,
    /// 当前可见终端行文本。
    pub lines: Vec<TerminalScreenLine>,
    /// 当前终端文本选区。
    pub selection: Option<TerminalTextSelection>,
    /// 需要绘制的连续单元格片段。
    pub runs: Vec<TerminalCellRun>,
}

/// 终端颜色值，覆盖默认色、索引色和 truecolor。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminalColor {
    /// 使用主题默认前景或背景色。
    Default,
    /// ANSI/256 色索引。
    Indexed(u8),
    /// 远端输出的 RGB truecolor。
    Rgb(u8, u8, u8),
}

impl From<vt100::Color> for TerminalColor {
    /// 将 vt100 的颜色表示转换为 Argus UI 层可独立消费的颜色类型。
    fn from(color: vt100::Color) -> Self {
        match color {
            vt100::Color::Default => Self::Default,
            vt100::Color::Idx(index) => Self::Indexed(index),
            vt100::Color::Rgb(red, green, blue) => Self::Rgb(red, green, blue),
        }
    }
}

/// 终端单元格样式，包含文本色、背景色和常用 SGR 属性。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminalCellStyle {
    /// 单元格前景色。
    pub fg: TerminalColor,
    /// 单元格背景色。
    pub bg: TerminalColor,
    /// 是否使用粗体。
    pub is_bold: bool,
    /// 是否使用暗淡效果。
    pub is_dim: bool,
    /// 是否使用下划线。
    pub is_underline: bool,
    /// 是否反转前景色和背景色。
    pub is_inverse: bool,
}

impl TerminalCellStyle {
    /// 从 vt100 单元格中提取渲染所需样式。
    fn from_cell(cell: &vt100::Cell) -> Self {
        Self {
            fg: cell.fgcolor().into(),
            bg: cell.bgcolor().into(),
            is_bold: cell.bold(),
            is_dim: cell.dim(),
            is_underline: cell.underline(),
            is_inverse: cell.inverse(),
        }
    }
}

/// 同一行、同一样式的连续终端单元格片段。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TerminalCellRun {
    /// 片段所在行。
    pub row: u16,
    /// 片段起始列。
    pub start_col: u16,
    /// 片段占用的终端列数。
    pub cols: u16,
    /// 片段文本；空白背景片段会包含等宽空格。
    pub text: String,
    /// 片段样式。
    pub style: TerminalCellStyle,
}

/// 合并相邻且样式相同的终端单元格，降低 UI 绘制时的 text shaping 次数。
fn append_terminal_run(
    active_run: &mut Option<TerminalCellRun>,
    runs: &mut Vec<TerminalCellRun>,
    row: u16,
    start_col: u16,
    cols: u16,
    text: &str,
    style: TerminalCellStyle,
) {
    if let Some(run) = active_run
        && run.row == row
        && run.style == style
        && run.start_col + run.cols == start_col
    {
        run.cols += cols;
        run.text.push_str(text);
        return;
    }
    flush_terminal_run(active_run, runs);
    *active_run = Some(TerminalCellRun {
        row,
        start_col,
        cols,
        text: text.to_string(),
        style,
    });
}

/// 把正在构建的终端片段写入快照结果。
fn flush_terminal_run(active_run: &mut Option<TerminalCellRun>, runs: &mut Vec<TerminalCellRun>) {
    if let Some(run) = active_run.take() {
        runs.push(run);
    }
}

/// 将 vt100 屏幕的一行单元格转换为可渲染文本，并记录终端列到字节位置的映射。
fn terminal_screen_line(screen: &vt100::Screen, row: u16, cols: u16) -> TerminalScreenLine {
    let mut text = String::new();
    let mut column_byte_indices = vec![0; usize::from(cols) + 1];
    let mut col = 0;

    while col < cols {
        let start_byte = text.len();
        column_byte_indices[usize::from(col)] = start_byte;

        let Some(cell) = screen.cell(row, col) else {
            text.push(' ');
            col += 1;
            column_byte_indices[usize::from(col)] = text.len();
            continue;
        };

        if cell.is_wide_continuation() {
            // 宽字符的续列已经由前一个单元格文本承担视觉宽度，这里只补齐列边界映射。
            col += 1;
            column_byte_indices[usize::from(col)] = text.len();
            continue;
        }

        if cell.has_contents() {
            text.push_str(cell.contents());
        } else {
            text.push(' ');
        }

        let cell_cols = if cell.is_wide() { 2 } else { 1 };
        let end_col = col.saturating_add(cell_cols).min(cols);
        for boundary_col in col + 1..=end_col {
            column_byte_indices[usize::from(boundary_col)] = text.len();
        }
        col = end_col;
    }

    TerminalScreenLine {
        text,
        column_byte_indices,
    }
}

/// 终端会话状态，用于右侧面板展示不同文案和操作。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TerminalStatus {
    /// 正在连接服务器。
    Connecting,
    /// 已拿到未知主机指纹，等待用户确认。
    AwaitingHostKey,
    /// SSH shell 已经建立，可输入命令。
    Connected,
    /// 用户主动断开或远端关闭。
    Disconnected,
    /// 连接或鉴权失败。
    Failed,
}

/// 等待用户确认的主机指纹信息。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingHostKey {
    /// 远程主机。
    pub host: String,
    /// 远程端口。
    pub port: u16,
    /// 待确认的 SHA256 指纹。
    pub fingerprint: String,
}

/// 启动 SSH worker 时需要的不可变请求数据。
#[derive(Clone, Debug)]
pub(crate) struct TerminalWorkerRequest {
    /// 终端会话 ID。
    pub session_id: usize,
    /// SSH 配置快照。
    pub ssh: SshLinkConfig,
    /// 已保存的可信指纹；为空表示首次连接需要用户确认。
    pub trusted_fingerprint: Option<String>,
    /// 初始远程 PTY 行数。
    pub rows: u16,
    /// 初始远程 PTY 列数。
    pub cols: u16,
}

/// 远程 PTY 尺寸；主机指纹确认期间的 resize 会先缓存到这里。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TerminalPtySize {
    /// 远程 PTY 行数。
    rows: u16,
    /// 远程 PTY 列数。
    cols: u16,
}

impl TerminalPtySize {
    /// 根据 worker 初始请求创建 PTY 尺寸快照。
    fn from_request(request: &TerminalWorkerRequest) -> Self {
        Self {
            rows: request.rows.max(1),
            cols: request.cols.max(1),
        }
    }
}

/// UI 发送给 SSH worker 的命令。
#[derive(Clone, Debug)]
pub(crate) enum TerminalCommand {
    /// 用户确认当前未知主机指纹可信。
    TrustHostKey,
    /// 用户拒绝当前未知主机指纹。
    RejectHostKey,
    /// 写入远程 shell 的原始字节。
    Input(Vec<u8>),
    /// 调整远程 PTY 尺寸。
    Resize {
        /// 行数。
        rows: u16,
        /// 列数。
        cols: u16,
    },
    /// 主动断开 SSH 通道。
    Disconnect,
}

/// SSH worker 回传给 UI 的事件。
#[derive(Clone, Debug)]
pub(crate) enum TerminalEvent {
    /// 发现未知主机指纹，需要 UI 弹窗确认。
    HostKeyVerification {
        /// 会话 ID。
        session_id: usize,
        /// 主机。
        host: String,
        /// 端口。
        port: u16,
        /// SHA256 指纹。
        fingerprint: String,
    },
    /// SSH shell 已连接。
    Connected {
        /// 会话 ID。
        session_id: usize,
    },
    /// 收到远程输出。
    Output {
        /// 会话 ID。
        session_id: usize,
        /// 原始终端字节。
        bytes: Vec<u8>,
    },
    /// 远端或本地已经断开。
    Disconnected {
        /// 会话 ID。
        session_id: usize,
        /// 用户可读原因。
        message: String,
    },
    /// SSH worker 失败。
    Failed {
        /// 会话 ID。
        session_id: usize,
        /// 用户可读原因。
        message: String,
    },
}

/// 启动 SSH 后台线程，并返回命令发送端与事件接收端。
pub(crate) fn spawn_ssh_worker(
    request: TerminalWorkerRequest,
) -> (mpsc::Sender<TerminalCommand>, Receiver<TerminalEvent>) {
    let (command_sender, command_receiver) = mpsc::channel();
    let (event_sender, event_receiver) = async_channel::unbounded();
    thread::spawn(move || {
        if let Err(error) = run_ssh_worker(request.clone(), command_receiver, event_sender.clone())
        {
            send_event_blocking(
                &event_sender,
                TerminalEvent::Failed {
                    session_id: request.session_id,
                    message: error.to_string(),
                },
            );
        }
    });
    (command_sender, event_receiver)
}

/// 根据 GPUI 按键字段生成写入终端的字节序列。
pub(crate) fn terminal_input_bytes(
    key: &str,
    key_char: Option<&str>,
    is_control: bool,
    is_platform: bool,
) -> Option<Vec<u8>> {
    if is_platform {
        return None;
    }
    if is_control {
        return match key {
            "c" => Some(vec![0x03]),
            "d" => Some(vec![0x04]),
            _ => None,
        };
    }

    match key {
        "enter" => Some(b"\r".to_vec()),
        "backspace" => Some(vec![0x7f]),
        "tab" => Some(b"\t".to_vec()),
        "escape" => Some(vec![0x1b]),
        "up" | "arrowup" => Some(b"\x1b[A".to_vec()),
        "down" | "arrowdown" => Some(b"\x1b[B".to_vec()),
        "right" | "arrowright" => Some(b"\x1b[C".to_vec()),
        "left" | "arrowleft" => Some(b"\x1b[D".to_vec()),
        _ => key_char
            .filter(|text| !text.chars().any(char::is_control))
            .map(|text| text.as_bytes().to_vec()),
    }
}

/// SSH worker 主流程：连接、校验主机指纹、鉴权、建立 shell 并转发输入输出。
fn run_ssh_worker(
    request: TerminalWorkerRequest,
    command_receiver: mpsc::Receiver<TerminalCommand>,
    event_sender: Sender<TerminalEvent>,
) -> Result<()> {
    let (session, fingerprint) = connect_ssh_session(&request.ssh)?;
    let pty_size = verify_host_key(&request, &command_receiver, &event_sender, &fingerprint)?;
    authenticate_ssh_session(&session, &request.ssh)?;

    let mut channel = session
        .channel_session()
        .context("无法创建 SSH shell 通道")?;
    channel
        .request_pty(
            "xterm-256color",
            None,
            Some((pty_size.cols as u32, pty_size.rows as u32, 0, 0)),
        )
        .context("远程 PTY 申请失败")?;
    channel.shell().context("远程 shell 启动失败")?;
    send_event_blocking(
        &event_sender,
        TerminalEvent::Connected {
            session_id: request.session_id,
        },
    );

    session.set_blocking(false);
    let mut buffer = [0_u8; 8192];
    loop {
        while let Ok(command) = command_receiver.try_recv() {
            match command {
                TerminalCommand::TrustHostKey | TerminalCommand::RejectHostKey => {}
                TerminalCommand::Input(bytes) => write_channel_bytes(&mut channel, &bytes)?,
                TerminalCommand::Resize { rows, cols } => {
                    let _ = channel.request_pty_size(cols as u32, rows as u32, None, None);
                }
                TerminalCommand::Disconnect => {
                    let _ = channel.close();
                    send_event_blocking(
                        &event_sender,
                        TerminalEvent::Disconnected {
                            session_id: request.session_id,
                            message: "SSH 连接已断开".to_string(),
                        },
                    );
                    return Ok(());
                }
            }
        }

        match channel.read(&mut buffer) {
            Ok(0) if channel.eof() => break,
            Ok(0) => thread::sleep(SSH_WORKER_IDLE_SLEEP),
            Ok(read_len) => {
                send_event_blocking(
                    &event_sender,
                    TerminalEvent::Output {
                        session_id: request.session_id,
                        bytes: buffer[..read_len].to_vec(),
                    },
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(SSH_WORKER_IDLE_SLEEP)
            }
            Err(error) => return Err(anyhow!("SSH 输出读取失败：{error}")),
        }
    }

    let _ = channel.wait_close();
    send_event_blocking(
        &event_sender,
        TerminalEvent::Disconnected {
            session_id: request.session_id,
            message: "远程 shell 已关闭".to_string(),
        },
    );
    Ok(())
}

/// 校验主机指纹；未知主机需要等待 UI 发送确认或拒绝命令。
fn verify_host_key(
    request: &TerminalWorkerRequest,
    command_receiver: &mpsc::Receiver<TerminalCommand>,
    event_sender: &Sender<TerminalEvent>,
    fingerprint: &str,
) -> Result<TerminalPtySize> {
    let mut pty_size = TerminalPtySize::from_request(request);
    match request.trusted_fingerprint.as_deref() {
        Some(expected) if expected == fingerprint => Ok(pty_size),
        Some(_) => bail!("SSH 主机指纹发生变化，已阻止连接"),
        None => {
            send_event_blocking(
                event_sender,
                TerminalEvent::HostKeyVerification {
                    session_id: request.session_id,
                    host: request.ssh.host.clone(),
                    port: request.ssh.port,
                    fingerprint: fingerprint.to_string(),
                },
            );
            loop {
                match command_receiver.recv() {
                    Ok(TerminalCommand::TrustHostKey) => return Ok(pty_size),
                    Ok(TerminalCommand::RejectHostKey) => {
                        bail!("用户拒绝信任 SSH 主机指纹")
                    }
                    Ok(TerminalCommand::Disconnect) | Err(_) => bail!("SSH 连接已取消"),
                    Ok(TerminalCommand::Resize { rows, cols }) => {
                        // 指纹确认前还没有远程 channel，先记住最终尺寸，确认后申请正确 PTY。
                        pty_size = TerminalPtySize {
                            rows: rows.max(1),
                            cols: cols.max(1),
                        };
                    }
                    Ok(TerminalCommand::Input(_)) => {
                        // 指纹确认前不应写入远程终端；丢弃这些输入可避免误发密码或命令。
                    }
                }
            }
        }
    }
}

/// 向远程通道写入字节，非阻塞模式下遇到 WouldBlock 时保留简洁失败提示。
fn write_channel_bytes(channel: &mut ssh2::Channel, bytes: &[u8]) -> Result<()> {
    channel
        .write_all(bytes)
        .map_err(|error| anyhow!("SSH 输入写入失败：{error}"))
}

/// 后台线程向 UI 事件通道发送消息；接收端关闭时直接忽略。
fn send_event_blocking(event_sender: &Sender<TerminalEvent>, event: TerminalEvent) {
    let _ = event_sender.send_blocking(event);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造不连接真实 SSH 的终端会话，便于验证 vt100 屏幕解析逻辑。
    fn terminal_session_for_test(rows: u16, cols: u16) -> TerminalSessionState {
        let (sender, _) = std::sync::mpsc::channel();
        TerminalSessionState {
            id: 1,
            link_id: 1,
            title: "测试终端".to_string(),
            status: TerminalStatus::Connected,
            parser: vt100::Parser::new(rows, cols, TERMINAL_SCROLLBACK_LINES),
            command_sender: Some(sender),
            pending_host_key: None,
            rows,
            cols,
            max_scrollback_offset: 0,
            scrollback_line_remainder: 0.0,
            scrollbar_drag_cursor_offset: None,
            viewport_scroll: ScrollHandle::new(),
            selection: None,
            selection_drag: None,
            message: None,
        }
    }

    /// 验证常用控制键会转换为终端预期字节。
    #[test]
    fn terminal_input_bytes_maps_control_keys() {
        assert_eq!(
            terminal_input_bytes("enter", None, false, false),
            Some(b"\r".to_vec())
        );
        assert_eq!(
            terminal_input_bytes("backspace", None, false, false),
            Some(vec![0x7f])
        );
        assert_eq!(
            terminal_input_bytes("up", None, false, false),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            terminal_input_bytes("c", Some("c"), true, false),
            Some(vec![0x03])
        );
    }

    /// 验证平台快捷键不会误发到远程终端。
    #[test]
    fn terminal_input_bytes_ignores_platform_shortcuts() {
        assert_eq!(terminal_input_bytes("c", Some("c"), false, true), None);
    }

    /// 验证屏幕快照保留 vt100 解析出的前景色、背景色和光标位置。
    #[test]
    fn screen_snapshot_preserves_color_background_and_cursor() {
        let mut session = terminal_session_for_test(3, 8);
        session.process_output(b"\x1b[31mR\x1b[44mB\x1b[0m");

        let snapshot = session.screen_snapshot();
        assert_eq!(snapshot.cursor_row, 0);
        assert_eq!(snapshot.cursor_col, 2);

        let red_run = snapshot
            .runs
            .iter()
            .find(|run| run.text == "R")
            .expect("应存在红色前景片段");
        assert_eq!(red_run.style.fg, TerminalColor::Indexed(1));
        assert_eq!(red_run.style.bg, TerminalColor::Default);

        let blue_background_run = snapshot
            .runs
            .iter()
            .find(|run| run.text == "B")
            .expect("应存在蓝色背景片段");
        assert_eq!(blue_background_run.style.fg, TerminalColor::Indexed(1));
        assert_eq!(blue_background_run.style.bg, TerminalColor::Indexed(4));
    }

    /// 验证远端应用隐藏光标时，UI 快照能保留该状态。
    #[test]
    fn screen_snapshot_preserves_hidden_cursor_flag() {
        let mut session = terminal_session_for_test(3, 8);
        session.process_output(b"\x1b[?25l");

        assert!(session.screen_snapshot().is_cursor_hidden);
    }

    /// 验证滚轮能进入 scrollback 历史，并在历史视图中隐藏实时光标。
    #[test]
    fn scroll_scrollback_moves_into_history_and_hides_cursor() {
        let mut session = terminal_session_for_test(3, 12);
        for index in 0..8 {
            session.process_output(format!("line-{index}\r\n").as_bytes());
        }

        assert_eq!(session.screen_snapshot().scrollback_offset, 0);
        assert!(session.scroll_scrollback_by(2.0));
        let snapshot = session.screen_snapshot();
        assert!(snapshot.scrollback_offset > 0);
        assert!(snapshot.max_scrollback_offset >= snapshot.scrollback_offset);
        assert!(snapshot.is_cursor_hidden);
    }

    /// 验证回到底部会清除 scrollback 偏移。
    #[test]
    fn reset_scrollback_returns_to_live_screen() {
        let mut session = terminal_session_for_test(3, 12);
        for index in 0..8 {
            session.process_output(format!("line-{index}\r\n").as_bytes());
        }
        session.scroll_scrollback_by(2.0);

        assert!(session.reset_scrollback());
        assert_eq!(session.screen_snapshot().scrollback_offset, 0);
    }

    /// 验证触控板这类精确滚轮的小步滚动会累计到满一行后生效。
    #[test]
    fn scroll_scrollback_accumulates_fractional_lines() {
        let mut session = terminal_session_for_test(3, 12);
        for index in 0..8 {
            session.process_output(format!("line-{index}\r\n").as_bytes());
        }

        assert!(!session.scroll_scrollback_by(0.4));
        assert!(session.scroll_scrollback_by(0.7));
        assert_eq!(session.screen_snapshot().scrollback_offset, 1);
    }

    /// 验证滚动条拖拽状态可以正确开始和结束。
    #[test]
    fn terminal_scrollbar_drag_state_round_trips() {
        let mut session = terminal_session_for_test(3, 12);

        session.begin_scrollbar_drag(6.5);
        assert_eq!(session.scrollbar_drag_cursor_offset(), Some(6.5));
        assert!(session.finish_scrollbar_drag());
        assert_eq!(session.scrollbar_drag_cursor_offset(), None);
        assert!(!session.finish_scrollbar_drag());
    }

    /// 验证终端快照会提供按列补齐的可见行文本，供元素化渲染保持等宽布局。
    #[test]
    fn screen_snapshot_contains_padded_lines() {
        let mut session = terminal_session_for_test(2, 6);
        session.process_output(b"ab");

        let snapshot = session.screen_snapshot();
        assert_eq!(snapshot.lines[0].text, "ab    ");
        assert_eq!(snapshot.lines[0].byte_range_for_columns(1..3), 1..3);
    }

    /// 验证终端文本选区可以按行列提取要复制的内容。
    #[test]
    fn terminal_selection_returns_selected_text() {
        let mut session = terminal_session_for_test(2, 8);
        session.process_output(b"hello");

        session.begin_selection(TerminalGridPosition { row: 0, col: 1 });
        assert!(session.update_selection(TerminalGridPosition { row: 0, col: 4 }));
        assert_eq!(session.selected_text().as_deref(), Some("ell"));
        assert!(session.finish_selection());
        assert!(session.selection.is_some());
    }

    /// 验证双击终端正文会选中当前单词。
    #[test]
    fn double_click_selects_terminal_word() {
        let mut session = terminal_session_for_test(2, 24);
        session.process_output(b"deploy app_server-01");

        session.begin_selection_with_click_count(TerminalGridPosition { row: 0, col: 8 }, 2);

        assert_eq!(session.selected_text().as_deref(), Some("app_server-01"));
    }

    /// 验证三击终端正文会选中整行内容。
    #[test]
    fn triple_click_selects_terminal_line() {
        let mut session = terminal_session_for_test(2, 24);
        session.process_output(b"deploy app_server-01");

        session.begin_selection_with_click_count(TerminalGridPosition { row: 0, col: 8 }, 3);

        assert_eq!(
            session.selected_text().as_deref(),
            Some("deploy app_server-01")
        );
    }

    /// 验证平台全选入口会选中当前可见终端屏幕。
    #[test]
    fn select_visible_screen_marks_full_terminal_view() {
        let mut session = terminal_session_for_test(2, 8);
        session.process_output(b"hello\r\nworld");

        assert!(session.select_visible_screen());
        let selection = session.selection.expect("应产生终端选区");
        assert_eq!(selection.anchor, TerminalGridPosition { row: 0, col: 0 });
        assert_eq!(selection.focus, TerminalGridPosition { row: 1, col: 8 });
        assert!(
            session
                .selected_text()
                .expect("全选后应能复制文本")
                .contains("hello")
        );
    }

    /// 验证未知主机确认前收到的 resize 会用于后续远程 PTY 申请。
    #[test]
    fn verify_host_key_preserves_resize_before_trust() {
        let request = TerminalWorkerRequest {
            session_id: 7,
            ssh: SshLinkConfig {
                host: "10.0.0.1".to_string(),
                port: 22,
                username: "deploy".to_string(),
                password: "secret".to_string(),
                private_key_path: None,
                private_key_passphrase: None,
            },
            trusted_fingerprint: None,
            rows: DEFAULT_TERMINAL_ROWS,
            cols: DEFAULT_TERMINAL_COLS,
        };
        let (command_sender, command_receiver) = std::sync::mpsc::channel();
        let (event_sender, event_receiver) = async_channel::unbounded();
        command_sender
            .send(TerminalCommand::Resize {
                rows: 42,
                cols: 132,
            })
            .unwrap();
        command_sender.send(TerminalCommand::TrustHostKey).unwrap();

        let pty_size =
            verify_host_key(&request, &command_receiver, &event_sender, "SHA256:test").unwrap();

        assert_eq!(
            pty_size,
            TerminalPtySize {
                rows: 42,
                cols: 132
            }
        );
        assert!(matches!(
            event_receiver.try_recv().unwrap(),
            TerminalEvent::HostKeyVerification {
                session_id: 7,
                fingerprint,
                ..
            } if fingerprint == "SHA256:test"
        ));
    }
}
