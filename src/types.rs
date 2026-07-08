//! 文件职责：定义跨模块共享的基础类型，避免底层模块反向依赖应用层。
//! 创建日期：2026-07-08
//! 作者：Argus 开发团队
//! 主要功能：提供通用的文本输入框状态和拖拽选区类型，供 app、ui 和 remote 模块共同使用。

use crate::infra::text_selection::{TextSelectionGranularity, character_count};

/// 单行输入框拖拽选择状态，记录起始字符范围和当前拖拽粒度。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputTextSelectionDrag {
    /// 鼠标按下时形成的基础字符范围。
    pub anchor_range: std::ops::Range<usize>,
    /// 当前拖拽粒度，决定移动时按字符、词或整行扩展。
    pub granularity: TextSelectionGranularity,
}

/// 设置窗口中的单行输入框状态；用于保存持久化设置项的编辑光标和选区。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsTextInputState {
    /// 输入框当前文本。
    pub value: String,
    /// 光标字符位置。
    pub cursor: usize,
    /// 选区锚点；与光标不一致时表示存在选区。
    pub selection_anchor: Option<usize>,
    /// 输入法 marked text 字符范围，候选态替换时使用。
    pub marked_range: Option<std::ops::Range<usize>>,
    /// 鼠标拖拽选区状态。
    pub selection_drag: Option<InputTextSelectionDrag>,
    /// 是否处于焦点状态。
    pub is_focused: bool,
}

impl SettingsTextInputState {
    /// 根据已有配置值构造设置输入框状态，光标默认位于文本末尾。
    pub fn from_value(value: String) -> Self {
        let cursor = character_count(&value);
        Self {
            value,
            cursor,
            selection_anchor: None,
            marked_range: None,
            selection_drag: None,
            is_focused: false,
        }
    }
}

impl Default for SettingsTextInputState {
    /// 创建空设置输入框状态。
    fn default() -> Self {
        Self::from_value(String::new())
    }
}
