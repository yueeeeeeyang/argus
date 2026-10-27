//! 文件职责：把键盘、鼠标、粘贴与焦点事件编码为远端终端可解析的字节序列。
//! 创建日期：2026-10-15
//! 修改日期：2026-10-15
//! 作者：Argus 开发团队
//! 主要功能：按远端程序开启的输入模式（DECCKM/DECBKM/鼠标上报/括号粘贴）生成 xterm 兼容输入字节。

/// 单条终端输入命令的最大字节数；粘贴等大负载按此分块，避免单次分配与命令过大。
pub(crate) const TERMINAL_INPUT_CHUNK_BYTES: usize = 8 * 1024;

/// 终端输入模式快照；全部字段来自远端程序通过转义序列开启的模式。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct TerminalInputMode {
    /// 应用光标键模式（DECCKM）：方向键与 Home/End 使用 SS3 序列而不是 CSI 序列。
    pub application_cursor: bool,
    /// 回退键发送退格（DECBKM）：Backspace 发送 0x08 而不是 0x7F。
    pub backarrow_key_sends_backspace: bool,
    /// 括号粘贴模式（2004）：粘贴内容用 `CSI 200~` / `CSI 201~` 包裹。
    pub bracketed_paste: bool,
    /// 焦点事件上报（1004）。
    pub focus_events: bool,
    /// 鼠标上报模式（9/1000/1002/1003）。
    pub mouse_mode: TerminalMouseMode,
    /// 鼠标上报编码（1005/1006/1015，缺省为 X10 单字节编码）。
    pub mouse_encoding: TerminalMouseEncoding,
}

/// 鼠标上报模式；与 xterm 的 9/1000/1002/1003 模式一一对应。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TerminalMouseMode {
    /// 不上报鼠标；终端保留本地选择与滚动。
    #[default]
    None,
    /// 仅上报按下事件（X10 模式）。
    Press,
    /// 上报按下与释放事件（VT200 模式）。
    PressRelease,
    /// 额外上报按住按钮时的移动事件（1002）。
    ButtonMotion,
    /// 上报全部移动事件（1003）。
    AnyMotion,
}

/// 鼠标上报编码。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TerminalMouseEncoding {
    /// X10 单字节编码：坐标上限 223。
    #[default]
    X10,
    /// UTF-8 编码（1005）。
    Utf8,
    /// SGR 编码（1006）：现代程序默认请求的编码。
    Sgr,
    /// urxvt 扩展编码（1015）。
    Urxvt,
}

/// 终端按键修饰键快照；语义与 xterm 修饰键编码一致。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct TerminalKeyModifiers {
    /// Shift。
    pub shift: bool,
    /// Alt/Option。
    pub alt: bool,
    /// Control。
    pub control: bool,
    /// Cmd/Super：属于本地平台快捷键，不下发远端。
    pub platform: bool,
    /// Fn：平台层功能键，不下发远端。
    pub function: bool,
}

/// 一次按键输入。
pub(crate) struct TerminalKeyInput<'a> {
    /// GPUI 规范化按键名，例如 `up`、`f5`、`a`。
    pub key: &'a str,
    /// 平台按键字符；方向键等特殊键为空。
    pub key_char: Option<&'a str>,
    /// 修饰键快照。
    pub modifiers: TerminalKeyModifiers,
    /// Option/Alt 与普通字符组合时是否发送 ESC 前缀（「Option 键作为 Meta」设置）。
    pub option_as_meta: bool,
}

/// 鼠标事件类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminalMouseEventKind {
    /// 按下。
    Press,
    /// 释放。
    Release,
    /// 移动。
    Motion,
}

/// 鼠标按钮。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminalMouseButton {
    /// 左键。
    Left,
    /// 中键。
    Middle,
    /// 右键。
    Right,
    /// 滚轮上滚。
    WheelUp,
    /// 滚轮下滚。
    WheelDown,
    /// 滚轮左滚。
    WheelLeft,
    /// 滚轮右滚。
    WheelRight,
}

/// 一次终端鼠标事件；坐标是 1 基单元格坐标。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminalMouseEvent {
    /// 事件类型。
    pub kind: TerminalMouseEventKind,
    /// 事件按钮；纯移动（AnyMotion 模式不按住按钮）时为空。
    pub button: Option<TerminalMouseButton>,
    /// 修饰键快照。
    pub modifiers: TerminalKeyModifiers,
    /// 1 基单元格列号。
    pub col: u16,
    /// 1 基单元格行号。
    pub row: u16,
}

/// 把一次按键编码为终端输入字节；无需发送时返回 `None`。
///
/// 编码规则遵循 xterm 控制序列：
/// - 平台快捷键（Cmd）与 Fn 组合保持本地，不发送；
/// - Ctrl 组合优先映射为控制字节（Ctrl+A..Z → 0x01..0x1A 等）；
/// - 方向键与 Home/End 在应用光标键模式下发送 SS3 序列；
/// - 带修饰的方向/Home/End 与编辑键区使用 `CSI 1;P` / `CSI n;P~` 形式；
/// - Option 作为 Meta 开启时，Alt+普通字符发送 ESC 前缀而不是平台组合字符。
pub(crate) fn encode_key(input: &TerminalKeyInput<'_>, mode: TerminalInputMode) -> Option<Vec<u8>> {
    let modifiers = input.modifiers;
    if modifiers.platform || modifiers.function {
        return None;
    }
    let key = input.key.to_ascii_lowercase();
    let modifier_code = xterm_modifier_code(modifiers);

    if modifiers.control
        && let Some(byte) = control_byte_for_key(&key, modifiers.shift)
    {
        return Some(control_bytes(byte, input));
    }

    if let Some(bytes) = special_key_bytes(input, &key, modifier_code, mode) {
        return Some(bytes);
    }

    let text = input.key_char?;
    if text.chars().any(char::is_control) {
        return None;
    }
    if modifiers.alt
        && input.option_as_meta
        && let Some(base) = meta_base_character(&key, modifiers.shift)
    {
        // 发送 ESC + 基础字符，而不是平台组合字符（例如 Option+a 的 å）。
        let mut bytes = Vec::with_capacity(base.len_utf8() + 1);
        bytes.push(0x1b);
        let mut encoded = [0_u8; 4];
        bytes.extend_from_slice(base.encode_utf8(&mut encoded).as_bytes());
        return Some(bytes);
    }
    Some(text.as_bytes().to_vec())
}

/// 把粘贴文本编码为终端输入字节。
///
/// 括号粘贴模式下按 `CSI 200~` / `CSI 201~` 包裹并使用 LF 换行，让 vim 等程序把整段
/// 文本当作一次粘贴处理；未开启时退回逐行提交语义（换行统一为回车）。
pub(crate) fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let normalized = text.replace("\r\n", "\n");
        let mut bytes = Vec::with_capacity(normalized.len() + 12);
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(normalized.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        bytes
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// 把焦点变化编码为终端输入字节；远端未开启 1004 焦点事件时返回 `None`。
pub(crate) fn encode_focus(is_focused: bool, mode: TerminalInputMode) -> Option<Vec<u8>> {
    mode.focus_events.then(|| {
        if is_focused {
            b"\x1b[I".to_vec()
        } else {
            b"\x1b[O".to_vec()
        }
    })
}

/// 把鼠标事件编码为终端输入字节；远端未开启鼠标上报或模式不支持该事件时返回 `None`。
pub(crate) fn encode_mouse(event: &TerminalMouseEvent, mode: TerminalInputMode) -> Option<Vec<u8>> {
    if mode.mouse_mode == TerminalMouseMode::None {
        return None;
    }
    // 按上报模式过滤事件：X10 模式只上报按下，VT200 模式不上报移动。
    match (mode.mouse_mode, event.kind) {
        (
            TerminalMouseMode::Press,
            TerminalMouseEventKind::Release | TerminalMouseEventKind::Motion,
        )
        | (TerminalMouseMode::PressRelease, TerminalMouseEventKind::Motion) => return None,
        _ => {}
    }

    let is_release = event.kind == TerminalMouseEventKind::Release;
    let mut code = if is_release && mode.mouse_encoding != TerminalMouseEncoding::Sgr {
        // 传统编码的释放事件统一使用「无按钮」代码。
        3
    } else {
        event.button.map(button_code).unwrap_or(3)
    };
    if event.kind == TerminalMouseEventKind::Motion {
        code += 32;
    }
    code += mouse_modifier_bits(event.modifiers);

    match mode.mouse_encoding {
        TerminalMouseEncoding::Sgr => {
            let final_byte = if is_release { 'm' } else { 'M' };
            Some(format!("\x1b[<{code};{};{}{final_byte}", event.col, event.row).into_bytes())
        }
        TerminalMouseEncoding::Urxvt => {
            Some(format!("\x1b[{};{};{}M", code + 32, event.col, event.row).into_bytes())
        }
        TerminalMouseEncoding::Utf8 => legacy_mouse_bytes(code, event.col, event.row, true),
        TerminalMouseEncoding::X10 => legacy_mouse_bytes(code, event.col, event.row, false),
    }
}

/// 按分块大小切分终端输入字节；顺序保持与原字节流一致。
pub(crate) fn terminal_input_chunks(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.chunks(TERMINAL_INPUT_CHUNK_BYTES)
}

/// xterm 修饰键参数：`1 + Shift 1 + Alt 2 + Ctrl 4`。
fn xterm_modifier_code(modifiers: TerminalKeyModifiers) -> u8 {
    1 + u8::from(modifiers.shift) + 2 * u8::from(modifiers.alt) + 4 * u8::from(modifiers.control)
}

/// Ctrl 组合对应的控制字节；遵循 xterm 的 Ctrl+字母与 Ctrl+符号映射。
fn control_byte_for_key(key: &str, shift: bool) -> Option<u8> {
    match key {
        // 空格与 Ctrl+2 发送 NUL。
        "space" | "@" | "2" => return Some(0x00),
        "3" => return Some(0x1b),
        "4" => return Some(0x1c),
        "5" => return Some(0x1d),
        "6" => return Some(0x1e),
        "7" => return Some(0x1f),
        // Ctrl+8 与 Ctrl+Shift+/（即 Ctrl+?）发送 DEL。
        "8" => return Some(0x7f),
        "/" if shift => return Some(0x7f),
        _ => {}
    }
    let mut chars = key.chars();
    let character = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    if character.is_ascii_lowercase() {
        return Some(character as u8 - b'a' + 1);
    }
    match character {
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        '?' => Some(0x7f),
        _ => None,
    }
}

/// 控制字节编码；「Option 作为 Meta」开启时 Alt 组合发送 ESC 前缀。
fn control_bytes(byte: u8, input: &TerminalKeyInput<'_>) -> Vec<u8> {
    if input.modifiers.alt && input.option_as_meta {
        vec![0x1b, byte]
    } else {
        vec![byte]
    }
}

/// 特殊键（编辑键区、方向键、功能键）编码；普通字符返回 `None`。
fn special_key_bytes(
    input: &TerminalKeyInput<'_>,
    key: &str,
    modifier_code: u8,
    mode: TerminalInputMode,
) -> Option<Vec<u8>> {
    match key {
        "enter" => Some(if input.modifiers.alt && input.option_as_meta {
            b"\x1b\r".to_vec()
        } else {
            b"\r".to_vec()
        }),
        "escape" => Some(vec![0x1b]),
        "tab" => Some(if input.modifiers.shift {
            b"\x1b[Z".to_vec()
        } else {
            b"\t".to_vec()
        }),
        "backspace" => Some(vec![if mode.backarrow_key_sends_backspace {
            0x08
        } else {
            0x7f
        }]),
        "up" => Some(cursor_key_bytes(b'A', modifier_code, mode)),
        "down" => Some(cursor_key_bytes(b'B', modifier_code, mode)),
        "right" => Some(cursor_key_bytes(b'C', modifier_code, mode)),
        "left" => Some(cursor_key_bytes(b'D', modifier_code, mode)),
        "home" => Some(cursor_key_bytes(b'H', modifier_code, mode)),
        "end" => Some(cursor_key_bytes(b'F', modifier_code, mode)),
        "insert" => Some(tilde_key_bytes(2, modifier_code)),
        "delete" => Some(tilde_key_bytes(3, modifier_code)),
        "pageup" => Some(tilde_key_bytes(5, modifier_code)),
        "pagedown" => Some(tilde_key_bytes(6, modifier_code)),
        _ => function_key_bytes(key, modifier_code),
    }
}

/// 方向键与 Home/End 编码：应用光标键模式下未带修饰时使用 SS3，带修饰时始终使用 CSI。
fn cursor_key_bytes(final_byte: u8, modifier_code: u8, mode: TerminalInputMode) -> Vec<u8> {
    if modifier_code == 1 {
        if mode.application_cursor {
            vec![0x1b, b'O', final_byte]
        } else {
            vec![0x1b, b'[', final_byte]
        }
    } else {
        format!("\x1b[1;{modifier_code}{}", final_byte as char).into_bytes()
    }
}

/// `CSI n ~` 形式按键编码；带修饰时使用 `CSI n;P ~`。
fn tilde_key_bytes(code: u16, modifier_code: u8) -> Vec<u8> {
    if modifier_code == 1 {
        format!("\x1b[{code}~").into_bytes()
    } else {
        format!("\x1b[{code};{modifier_code}~").into_bytes()
    }
}

/// 功能键编码：F1-F4 使用 SS3 P/Q/R/S，F5-F20 使用 `CSI n ~`。
fn function_key_bytes(key: &str, modifier_code: u8) -> Option<Vec<u8>> {
    let number = key.strip_prefix('f')?.parse::<u32>().ok()?;
    match number {
        1..=4 => {
            let final_byte = b'P' + (number as u8 - 1);
            Some(if modifier_code == 1 {
                vec![0x1b, b'O', final_byte]
            } else {
                format!("\x1b[1;{modifier_code}{}", final_byte as char).into_bytes()
            })
        }
        5 => Some(tilde_key_bytes(15, modifier_code)),
        6 => Some(tilde_key_bytes(17, modifier_code)),
        7 => Some(tilde_key_bytes(18, modifier_code)),
        8 => Some(tilde_key_bytes(19, modifier_code)),
        9 => Some(tilde_key_bytes(20, modifier_code)),
        10 => Some(tilde_key_bytes(21, modifier_code)),
        11 => Some(tilde_key_bytes(23, modifier_code)),
        12 => Some(tilde_key_bytes(24, modifier_code)),
        13 => Some(tilde_key_bytes(25, modifier_code)),
        14 => Some(tilde_key_bytes(26, modifier_code)),
        15 => Some(tilde_key_bytes(28, modifier_code)),
        16 => Some(tilde_key_bytes(29, modifier_code)),
        17 => Some(tilde_key_bytes(31, modifier_code)),
        18 => Some(tilde_key_bytes(32, modifier_code)),
        19 => Some(tilde_key_bytes(33, modifier_code)),
        20 => Some(tilde_key_bytes(34, modifier_code)),
        _ => None,
    }
}

/// 「Option 作为 Meta」使用的基础字符；只对单字符键与空格生效。
fn meta_base_character(key: &str, shift: bool) -> Option<char> {
    if key == "space" {
        return Some(' ');
    }
    let mut chars = key.chars();
    let character = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    if shift {
        Some(character.to_ascii_uppercase())
    } else {
        Some(character)
    }
}

/// 鼠标按钮基础代码。
fn button_code(button: TerminalMouseButton) -> u8 {
    match button {
        TerminalMouseButton::Left => 0,
        TerminalMouseButton::Middle => 1,
        TerminalMouseButton::Right => 2,
        TerminalMouseButton::WheelUp => 64,
        TerminalMouseButton::WheelDown => 65,
        TerminalMouseButton::WheelLeft => 66,
        TerminalMouseButton::WheelRight => 67,
    }
}

/// 鼠标修饰键位：Shift 4、Alt 8、Ctrl 16。
fn mouse_modifier_bits(modifiers: TerminalKeyModifiers) -> u8 {
    4 * u8::from(modifiers.shift) + 8 * u8::from(modifiers.alt) + 16 * u8::from(modifiers.control)
}

/// X10 单字节 / UTF-8 鼠标编码；X10 坐标超出 223 时丢弃事件。
fn legacy_mouse_bytes(code: u8, col: u16, row: u16, use_utf8: bool) -> Option<Vec<u8>> {
    let mut bytes = vec![0x1b, b'[', b'M', code + 32];
    if use_utf8 {
        let encoded_col = char::from_u32(u32::from(col) + 32)?;
        let encoded_row = char::from_u32(u32::from(row) + 32)?;
        let mut buffer = [0_u8; 4];
        bytes.extend_from_slice(encoded_col.encode_utf8(&mut buffer).as_bytes());
        bytes.extend_from_slice(encoded_row.encode_utf8(&mut buffer).as_bytes());
        Some(bytes)
    } else {
        // X10 编码把坐标偏移 32 后写入单个字节，最大可表达 223。
        let encoded_col = col
            .checked_add(32)
            .filter(|value| *value <= u8::MAX as u16)?;
        let encoded_row = row
            .checked_add(32)
            .filter(|value| *value <= u8::MAX as u16)?;
        bytes.push(encoded_col as u8);
        bytes.push(encoded_row as u8);
        Some(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造无修饰按键输入。
    fn key<'a>(key: &'a str, key_char: Option<&'a str>) -> TerminalKeyInput<'a> {
        TerminalKeyInput {
            key,
            key_char,
            modifiers: TerminalKeyModifiers::default(),
            option_as_meta: false,
        }
    }

    /// 构造带修饰与 Option-as-Meta 的按键输入。
    fn key_with<'a>(
        key_name: &'a str,
        key_char: Option<&'a str>,
        modifiers: TerminalKeyModifiers,
        option_as_meta: bool,
    ) -> TerminalKeyInput<'a> {
        TerminalKeyInput {
            key: key_name,
            key_char,
            modifiers,
            option_as_meta,
        }
    }

    /// 构造默认（无应用模式、无鼠标）的输入模式。
    fn mode() -> TerminalInputMode {
        TerminalInputMode::default()
    }

    /// 验证方向键在普通模式与应用光标键模式下的编码切换。
    #[test]
    fn arrow_keys_follow_application_cursor_mode() {
        let mut application = mode();
        application.application_cursor = true;

        assert_eq!(
            encode_key(&key("up", None), mode()),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode_key(&key("up", None), application),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            encode_key(&key("home", None), application),
            Some(b"\x1bOH".to_vec())
        );
        assert_eq!(
            encode_key(&key("end", None), mode()),
            Some(b"\x1b[F".to_vec())
        );
    }

    /// 验证带修饰的方向键始终使用 CSI 编码，即使应用光标键模式已开启。
    #[test]
    fn modified_cursor_keys_always_use_csi() {
        let mut application = mode();
        application.application_cursor = true;
        let ctrl_up = key_with(
            "up",
            None,
            TerminalKeyModifiers {
                control: true,
                ..Default::default()
            },
            false,
        );
        let shift_up = key_with(
            "up",
            None,
            TerminalKeyModifiers {
                shift: true,
                ..Default::default()
            },
            false,
        );

        assert_eq!(
            encode_key(&ctrl_up, application),
            Some(b"\x1b[1;5A".to_vec())
        );
        assert_eq!(
            encode_key(&shift_up, application),
            Some(b"\x1b[1;2A".to_vec())
        );
    }

    /// 验证编辑键区、功能键与 Shift+Tab 的编码表。
    #[test]
    fn editing_and_function_keys_match_xterm_table() {
        assert_eq!(
            encode_key(&key("insert", None), mode()),
            Some(b"\x1b[2~".to_vec())
        );
        assert_eq!(
            encode_key(&key("delete", None), mode()),
            Some(b"\x1b[3~".to_vec())
        );
        assert_eq!(
            encode_key(&key("pageup", None), mode()),
            Some(b"\x1b[5~".to_vec())
        );
        assert_eq!(
            encode_key(&key("pagedown", None), mode()),
            Some(b"\x1b[6~".to_vec())
        );
        assert_eq!(
            encode_key(&key("f1", None), mode()),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            encode_key(&key("f4", None), mode()),
            Some(b"\x1bOS".to_vec())
        );
        assert_eq!(
            encode_key(&key("f5", None), mode()),
            Some(b"\x1b[15~".to_vec())
        );
        assert_eq!(
            encode_key(&key("f12", None), mode()),
            Some(b"\x1b[24~".to_vec())
        );
        let shift_f5 = key_with(
            "f5",
            None,
            TerminalKeyModifiers {
                shift: true,
                ..Default::default()
            },
            false,
        );
        assert_eq!(encode_key(&shift_f5, mode()), Some(b"\x1b[15;2~".to_vec()));
        let shift_tab = key_with(
            "tab",
            Some("\t"),
            TerminalKeyModifiers {
                shift: true,
                ..Default::default()
            },
            false,
        );
        assert_eq!(encode_key(&shift_tab, mode()), Some(b"\x1b[Z".to_vec()));
    }

    /// 验证控制键映射覆盖全部字母与常见符号。
    #[test]
    fn control_keys_map_to_control_bytes() {
        let control = |name: &'static str| {
            key_with(
                name,
                Some(name),
                TerminalKeyModifiers {
                    control: true,
                    ..Default::default()
                },
                false,
            )
        };

        assert_eq!(encode_key(&control("a"), mode()), Some(vec![0x01]));
        assert_eq!(encode_key(&control("f"), mode()), Some(vec![0x06]));
        assert_eq!(encode_key(&control("z"), mode()), Some(vec![0x1a]));
        assert_eq!(encode_key(&control("space"), mode()), Some(vec![0x00]));
        assert_eq!(encode_key(&control("?"), mode()), Some(vec![0x7f]));
        // Ctrl 组合优先于特殊键表：Ctrl+回车仍发送回车。
        assert_eq!(encode_key(&control("enter"), mode()), Some(b"\r".to_vec()));
    }

    /// 验证 Backspace 跟随 DECBKM 模式，平台快捷键与 Fn 组合不下发。
    #[test]
    fn backspace_follows_backarrow_mode_and_platform_keys_stay_local() {
        let mut backarrow = mode();
        backarrow.backarrow_key_sends_backspace = true;
        assert_eq!(
            encode_key(&key("backspace", None), mode()),
            Some(vec![0x7f])
        );
        assert_eq!(
            encode_key(&key("backspace", None), backarrow),
            Some(vec![0x08])
        );

        let command_c = key_with(
            "c",
            Some("c"),
            TerminalKeyModifiers {
                platform: true,
                ..Default::default()
            },
            true,
        );
        let function_key = key_with(
            "f5",
            None,
            TerminalKeyModifiers {
                function: true,
                ..Default::default()
            },
            false,
        );
        assert_eq!(encode_key(&command_c, mode()), None);
        assert_eq!(encode_key(&function_key, mode()), None);
    }

    /// 验证 Option 作为 Meta 时发送 ESC 前缀，关闭时发送平台组合字符。
    #[test]
    fn option_as_meta_prefixes_escape() {
        let alt_a_with_meta = key_with(
            "a",
            Some("å"),
            TerminalKeyModifiers {
                alt: true,
                ..Default::default()
            },
            true,
        );
        let alt_a_without_meta = key_with(
            "a",
            Some("å"),
            TerminalKeyModifiers {
                alt: true,
                ..Default::default()
            },
            false,
        );
        let alt_shift_a = key_with(
            "a",
            Some("Å"),
            TerminalKeyModifiers {
                alt: true,
                shift: true,
                ..Default::default()
            },
            true,
        );

        assert_eq!(
            encode_key(&alt_a_with_meta, mode()),
            Some(b"\x1ba".to_vec())
        );
        assert_eq!(
            encode_key(&alt_a_without_meta, mode()),
            Some("å".as_bytes().to_vec())
        );
        assert_eq!(encode_key(&alt_shift_a, mode()), Some(b"\x1bA".to_vec()));
    }

    /// 验证括号粘贴使用 LF 包裹，非括号粘贴保持逐行回车语义。
    #[test]
    fn paste_encoding_follows_bracketed_paste_mode() {
        assert_eq!(
            encode_paste("ls\r\npwd\n", true),
            b"\x1b[200~ls\npwd\n\x1b[201~".to_vec()
        );
        assert_eq!(encode_paste("ls\r\npwd\n", false), b"ls\rpwd\r".to_vec());
        assert_eq!(
            encode_paste("single", true),
            b"\x1b[200~single\x1b[201~".to_vec()
        );
    }

    /// 验证焦点事件仅在远端开启 1004 时发送。
    #[test]
    fn focus_events_follow_mode() {
        let mut focused = mode();
        focused.focus_events = true;
        assert_eq!(encode_focus(true, focused), Some(b"\x1b[I".to_vec()));
        assert_eq!(encode_focus(false, focused), Some(b"\x1b[O".to_vec()));
        assert_eq!(encode_focus(true, mode()), None);
    }

    /// 验证 SGR 鼠标编码覆盖按下、释放、拖动、滚轮与修饰键。
    #[test]
    fn sgr_mouse_encoding_matches_xterm_sequence() {
        let mut sgr = mode();
        sgr.mouse_mode = TerminalMouseMode::PressRelease;
        sgr.mouse_encoding = TerminalMouseEncoding::Sgr;

        let press = TerminalMouseEvent {
            kind: TerminalMouseEventKind::Press,
            button: Some(TerminalMouseButton::Left),
            modifiers: TerminalKeyModifiers::default(),
            col: 10,
            row: 5,
        };
        assert_eq!(encode_mouse(&press, sgr), Some(b"\x1b[<0;10;5M".to_vec()));

        let release = TerminalMouseEvent {
            kind: TerminalMouseEventKind::Release,
            button: Some(TerminalMouseButton::Left),
            ..press
        };
        assert_eq!(encode_mouse(&release, sgr), Some(b"\x1b[<0;10;5m".to_vec()));

        let wheel = TerminalMouseEvent {
            kind: TerminalMouseEventKind::Press,
            button: Some(TerminalMouseButton::WheelUp),
            ..press
        };
        assert_eq!(encode_mouse(&wheel, sgr), Some(b"\x1b[<64;10;5M".to_vec()));

        let ctrl_drag = TerminalMouseEvent {
            kind: TerminalMouseEventKind::Motion,
            button: Some(TerminalMouseButton::Left),
            modifiers: TerminalKeyModifiers {
                control: true,
                ..Default::default()
            },
            col: 3,
            row: 4,
        };
        let mut motion = sgr;
        motion.mouse_mode = TerminalMouseMode::ButtonMotion;
        assert_eq!(
            encode_mouse(&ctrl_drag, motion),
            Some(b"\x1b[<48;3;4M".to_vec())
        );
    }

    /// 验证鼠标事件按上报模式过滤，且未开启鼠标模式时不产生字节。
    #[test]
    fn mouse_events_follow_reporting_mode() {
        let press = TerminalMouseEvent {
            kind: TerminalMouseEventKind::Press,
            button: Some(TerminalMouseButton::Left),
            modifiers: TerminalKeyModifiers::default(),
            col: 1,
            row: 1,
        };
        let release = TerminalMouseEvent {
            kind: TerminalMouseEventKind::Release,
            ..press
        };
        let motion = TerminalMouseEvent {
            kind: TerminalMouseEventKind::Motion,
            button: Some(TerminalMouseButton::Left),
            ..press
        };

        let mut x10 = mode();
        x10.mouse_mode = TerminalMouseMode::Press;
        assert!(encode_mouse(&press, x10).is_some());
        assert_eq!(encode_mouse(&release, x10), None);
        assert_eq!(encode_mouse(&motion, x10), None);

        let mut vt200 = mode();
        vt200.mouse_mode = TerminalMouseMode::PressRelease;
        assert!(encode_mouse(&release, vt200).is_some());
        assert_eq!(encode_mouse(&motion, vt200), None);

        assert_eq!(encode_mouse(&press, mode()), None);
    }

    /// 验证传统 X10 编码的坐标偏移与超限丢弃行为。
    #[test]
    fn legacy_mouse_encoding_offsets_coordinates() {
        let mut x10 = mode();
        x10.mouse_mode = TerminalMouseMode::PressRelease;

        let event = TerminalMouseEvent {
            kind: TerminalMouseEventKind::Press,
            button: Some(TerminalMouseButton::Right),
            modifiers: TerminalKeyModifiers::default(),
            col: 3,
            row: 4,
        };
        assert_eq!(
            encode_mouse(&event, x10),
            Some(vec![0x1b, b'[', b'M', 2 + 32, 3 + 32, 4 + 32])
        );

        let too_far = TerminalMouseEvent { col: 224, ..event };
        assert_eq!(encode_mouse(&too_far, x10), None);

        let release = TerminalMouseEvent {
            kind: TerminalMouseEventKind::Release,
            ..event
        };
        assert_eq!(
            encode_mouse(&release, x10),
            Some(vec![0x1b, b'[', b'M', 3 + 32, 3 + 32, 4 + 32])
        );
    }

    /// 验证输入分块保持字节顺序且不超过分块上限。
    #[test]
    fn terminal_input_chunks_preserve_order() {
        let bytes = vec![7_u8; TERMINAL_INPUT_CHUNK_BYTES + 10];
        let chunks = terminal_input_chunks(&bytes).collect::<Vec<_>>();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), TERMINAL_INPUT_CHUNK_BYTES);
        assert_eq!(chunks[1].len(), 10);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.iter().all(|byte| *byte == 7))
        );
    }
}
