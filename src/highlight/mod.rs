//! 文件职责：导出日志、配置文件与代码高亮模块的公共接口。
//! 创建日期：2026-06-11
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：集中声明高亮子模块，并向日志阅读区与文件预览暴露语言识别、缓存和高亮结果类型。

pub(crate) mod cache;
mod document;
pub(crate) mod highlighter;
pub(crate) mod language;
mod lexical;
pub(crate) mod rules;
pub(crate) mod span;

#[cfg(test)]
mod document_tests;
#[cfg(test)]
mod tests;

pub(crate) use cache::HighlightCache;
pub(crate) use document::DocumentHighlighter;
pub(crate) use highlighter::SyntaxHighlighter;
pub(crate) use language::{HighlightLanguage, detect_highlight_language};
pub(crate) use span::{HighlightSpan, HighlightTokenKind};

/// 判断语言是否需要跨行文档状态；日志阅读区据此决定是否构建文档级高亮缓存。
pub(crate) fn language_uses_document_state(language: HighlightLanguage) -> bool {
    lexical::uses_document_state(language)
}
