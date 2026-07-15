//! 文件职责：把语法高亮令牌统一解析为当前主题颜色。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：集中维护日志、代码预览和 Jstack 共享映射，并保留线程类令牌的产品差异。

use crate::highlight::HighlightTokenKind;
use crate::theme::AppTheme;

/// 高亮颜色的展示上下文；仅线程相关令牌在日志正文与线程分析中有意采用不同语义色。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HighlightColorContext {
    /// 常规日志正文，优先使用语法主题中的线程、方法和弱化位置颜色。
    Log,
    /// 只读文件预览，代码与配置令牌沿用编辑器语义色。
    FilePreview,
    /// Jstack 分析与详情，线程名/方法使用信息色，线程状态使用成功色。
    Jstack,
}

/// 根据令牌、主题和展示上下文返回最终 RGB 颜色。
pub(crate) fn color_for_highlight_token(
    kind: HighlightTokenKind,
    theme: &AppTheme,
    context: HighlightColorContext,
) -> u32 {
    match kind {
        HighlightTokenKind::Trace => theme.foreground_muted,
        HighlightTokenKind::Debug => theme.debug,
        HighlightTokenKind::Info => theme.info,
        HighlightTokenKind::Warning => theme.warning,
        HighlightTokenKind::Error | HighlightTokenKind::Fatal => theme.error,
        HighlightTokenKind::Timestamp => theme.syntax.timestamp,
        HighlightTokenKind::Comment => theme.syntax.comment,
        HighlightTokenKind::Key => theme.syntax.key,
        HighlightTokenKind::Value | HighlightTokenKind::String => theme.syntax.string,
        HighlightTokenKind::Number => theme.syntax.number,
        HighlightTokenKind::Boolean => theme.syntax.boolean,
        HighlightTokenKind::Punctuation => theme.syntax.punctuation,
        HighlightTokenKind::Tag => theme.syntax.tag,
        HighlightTokenKind::Attribute => theme.syntax.attribute,
        HighlightTokenKind::ThreadName => match context {
            HighlightColorContext::Log | HighlightColorContext::FilePreview => theme.syntax.thread,
            HighlightColorContext::Jstack => theme.info,
        },
        HighlightTokenKind::ThreadState => match context {
            HighlightColorContext::Log | HighlightColorContext::FilePreview => theme.warning,
            HighlightColorContext::Jstack => theme.success,
        },
        HighlightTokenKind::StackClass => theme.syntax.class,
        HighlightTokenKind::StackMethod => match context {
            HighlightColorContext::Log | HighlightColorContext::FilePreview => theme.syntax.method,
            HighlightColorContext::Jstack => theme.info,
        },
        HighlightTokenKind::StackLocation => match context {
            HighlightColorContext::Log | HighlightColorContext::FilePreview => {
                theme.foreground_muted
            }
            HighlightColorContext::Jstack => theme.syntax.string,
        },
        HighlightTokenKind::Lock => theme.syntax.lock,
        HighlightTokenKind::Exception => theme.syntax.exception,
        HighlightTokenKind::Keyword => theme.syntax.tag,
        HighlightTokenKind::Type => theme.syntax.class,
        HighlightTokenKind::Function => theme.syntax.method,
        HighlightTokenKind::Variable => theme.syntax.key,
        HighlightTokenKind::Operator => theme.syntax.punctuation,
        HighlightTokenKind::Annotation => theme.syntax.lock,
        HighlightTokenKind::Selector => theme.syntax.tag,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证普通令牌跨上下文保持一致，线程令牌继续保留清理前的有意差异。
    #[test]
    fn contexts_only_change_thread_related_colors() {
        let theme = AppTheme::dark();
        assert_eq!(
            color_for_highlight_token(
                HighlightTokenKind::Timestamp,
                &theme,
                HighlightColorContext::Log,
            ),
            color_for_highlight_token(
                HighlightTokenKind::Timestamp,
                &theme,
                HighlightColorContext::Jstack,
            )
        );
        assert_eq!(
            color_for_highlight_token(
                HighlightTokenKind::ThreadName,
                &theme,
                HighlightColorContext::Log,
            ),
            theme.syntax.thread
        );
        assert_eq!(
            color_for_highlight_token(
                HighlightTokenKind::ThreadState,
                &theme,
                HighlightColorContext::Jstack,
            ),
            theme.success
        );
    }
}
