//! 文件职责：为不可变预览文档保存每行进入状态，使虚拟列表随机访问仍获得一致高亮。

use std::sync::Arc;

use super::lexical::{LineState, advance_line_state, highlight_stateful_line, uses_document_state};
use super::{HighlightCache, HighlightLanguage, HighlightSpan, SyntaxHighlighter};

/// 文本、语言和顺序状态索引共同构成不可变快照；不同文档不共享行缓存。
#[derive(Clone, Debug)]
pub(crate) struct DocumentHighlighter {
    lines: Arc<Vec<String>>,
    language: HighlightLanguage,
    line_states: Arc<Vec<LineState>>,
    cache: HighlightCache,
}

impl DocumentHighlighter {
    /// 顺序扫描全文建立进入状态，不生成 token；构建工作量与文本字节数成正比。
    ///
    /// `lines` 必须是展示使用的同一份不可变行文本，换文档或语言时重新构建本对象。
    pub(crate) fn new(lines: Arc<Vec<String>>, language: HighlightLanguage) -> Self {
        let mut line_states = Vec::new();
        if uses_document_state(language) {
            line_states.reserve(lines.len());
            let mut state = LineState::default();
            for line in lines.iter() {
                line_states.push(state);
                state = advance_line_state(line, language, state);
            }
        }
        Self {
            lines,
            language,
            line_states: Arc::new(line_states),
            cache: HighlightCache::default(),
        }
    }

    /// 随机读取从零开始的行；超出文档范围返回空集合，显示 token 仍遵守单行扫描上限。
    pub(crate) fn highlight_line(&self, line_number: usize) -> Vec<HighlightSpan> {
        let Some(line) = self.lines.get(line_number) else {
            return Vec::new();
        };
        self.cache
            .highlight_indexed_line(line_number, self.language, || {
                if let Some(state) = self.line_states.get(line_number) {
                    highlight_stateful_line(line, self.language, *state)
                } else {
                    SyntaxHighlighter::highlight(line, self.language)
                }
            })
    }
}
