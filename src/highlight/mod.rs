//! 文件职责：导出日志与配置文件高亮模块的公共接口。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：集中声明高亮子模块，并向 UI 层暴露语言识别、缓存和高亮结果类型。

pub mod cache;
pub mod highlighter;
pub mod language;
pub mod rules;
pub mod span;

#[cfg(test)]
mod tests;

pub use cache::HighlightCache;
pub use highlighter::SyntaxHighlighter;
pub use language::{HighlightLanguage, detect_highlight_language};
pub use span::{HighlightSpan, HighlightTokenKind, MAX_HIGHLIGHT_BYTES};
