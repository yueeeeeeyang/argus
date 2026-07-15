//! 文件职责：导出日志、配置文件与代码高亮模块的公共接口。
//! 创建日期：2026-06-11
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：集中声明高亮子模块，并向日志阅读区与文件预览暴露语言识别、缓存和高亮结果类型。

pub(crate) mod cache;
pub(crate) mod highlighter;
pub(crate) mod language;
pub(crate) mod rules;
pub(crate) mod span;

#[cfg(test)]
mod tests;

pub(crate) use cache::HighlightCache;
pub(crate) use highlighter::SyntaxHighlighter;
pub(crate) use language::{HighlightLanguage, detect_highlight_language};
pub(crate) use span::{HighlightSpan, HighlightTokenKind};
