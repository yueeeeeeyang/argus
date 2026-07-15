//! 文件职责：提供语法高亮统一入口。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：根据语言类型分发到具体规则模块，并统一执行超长行扫描上限控制。

use crate::highlight::language::HighlightLanguage;
use crate::highlight::rules;
use crate::highlight::span::{HighlightSpan, SpanBuilder, capped_scan_len};

/// 纯逻辑高亮入口。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SyntaxHighlighter;

impl SyntaxHighlighter {
    /// 对单行展示文本执行高亮。
    ///
    /// 参数说明：
    /// - `line`：已经进入日志阅读区的展示文本，制表符应已展开。
    /// - `language`：当前 tab 根据文件名和路径识别出的语言。
    ///
    /// 返回值：不重叠且按起点排序的高亮范围。
    pub(crate) fn highlight(line: &str, language: HighlightLanguage) -> Vec<HighlightSpan> {
        if line.is_empty() || language == HighlightLanguage::Plain {
            return Vec::new();
        }

        let scan_len = capped_scan_len(line);
        let line = &line[..scan_len];
        let mut builder = SpanBuilder::new(line.len());
        match language {
            HighlightLanguage::Log => rules::log::highlight_log(line, &mut builder),
            HighlightLanguage::JavaThreadDump => {
                rules::java_thread::highlight_java_thread_dump(line, &mut builder)
            }
            HighlightLanguage::Properties => {
                rules::properties::highlight_properties(line, &mut builder)
            }
            HighlightLanguage::Xml => rules::xml::highlight_xml(line, &mut builder),
            HighlightLanguage::Json => rules::json::highlight_json(line, &mut builder),
            HighlightLanguage::Yaml => rules::yaml::highlight_yaml(line, &mut builder),
            HighlightLanguage::Plain => {}
        }
        builder.finish()
    }
}
