//! 文件职责：为 AI Agent 提供独立的多模式日志搜索执行器。
//! 创建日期：2026-07-17
//! 修改日期：2026-07-17
//! 作者：Argus 开发团队
//! 主要功能：编译关键字或正则、扫描 Agent 原生日志文档、累计完整计数并流式回调有限命中。

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use regex::{Regex, RegexBuilder};

use crate::agent::log_access::AgentLogAccess;
use crate::agent::session::SnapshotSource;

/// 单个 Agent 搜索模式；`pattern_id` 原样返回，避免工具层依赖查询文本去重。
#[derive(Clone, Debug)]
pub(crate) struct AgentSearchPattern {
    /// 调用方定义的稳定模式标识。
    pub pattern_id: String,
    /// 普通关键字或 Rust 正则表达式。
    pub query: String,
    /// 是否区分大小写。
    pub case_sensitive: bool,
    /// 是否按正则解释查询。
    pub regex: bool,
}

/// Agent 搜索命中的一行；正文仍留在本地工具层，是否发送模型由调用工具决定。
#[derive(Clone, Debug)]
pub(crate) struct AgentSearchHit {
    /// 当前会话不透明来源引用。
    pub source_ref: String,
    /// 多根范围内唯一展示路径。
    pub relative_path: String,
    /// 0 基行号。
    pub line_number: usize,
    /// 未脱敏原始行正文。
    pub line_text: String,
    /// 本行命中的模式 ID。
    pub matched_pattern_ids: Vec<String>,
}

/// 一次 Agent 原生搜索的完整本地统计。
#[derive(Clone, Debug, Default)]
pub(crate) struct AgentSearchSummary {
    /// 已尝试来源数。
    pub scanned_files: usize,
    /// 已扫描行数。
    pub scanned_lines: usize,
    /// 本次真正首次打开的原始字节数；缓存命中不重复计算。
    pub scanned_bytes: u64,
    /// 所有模式合并后的命中行数。
    pub matched_results: usize,
    /// 已脱离主循环的单来源读取错误。
    pub errors: Vec<String>,
    /// 是否在来源或行批次边界响应取消。
    pub was_cancelled: bool,
}

/// 编译后搜索模式；统一使用 `regex` 引擎保证普通关键字也具备稳定 Unicode 大小写语义。
struct CompiledAgentSearchPattern {
    /// 稳定模式标识。
    pattern_id: String,
    /// 编译后的逐行匹配器。
    matcher: Regex,
}

/// 不依赖主窗口搜索任务、结果面板或搜索状态的 Agent 专用搜索器。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AgentLogSearchEngine;

impl AgentLogSearchEngine {
    /// 只校验一个模式能否安全编译，不打开任何日志。
    pub(crate) fn validate_pattern(pattern: &AgentSearchPattern) -> Result<(), String> {
        compile_pattern(pattern).map(|_| ())
    }

    /// 扫描指定来源并把每条命中交给调用方；计数不受调用方保留条数影响。
    pub(crate) fn search(
        access: &AgentLogAccess,
        sources: &[SnapshotSource],
        patterns: &[AgentSearchPattern],
        cancel_flag: Arc<AtomicBool>,
        mut hit_callback: impl FnMut(AgentSearchHit),
    ) -> AgentSearchSummary {
        let mut summary = AgentSearchSummary::default();
        let compiled = match patterns
            .iter()
            .map(compile_pattern)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(compiled) => compiled,
            Err(error) => {
                summary.errors.push(error);
                return summary;
            }
        };

        for source in sources {
            if cancel_flag.load(Ordering::Relaxed) {
                summary.was_cancelled = true;
                break;
            }
            summary.scanned_files = summary.scanned_files.saturating_add(1);
            let opened = match access.open(&source.source_ref, cancel_flag.clone()) {
                Ok(opened) => opened,
                Err(error) => {
                    summary.errors.push(error.to_string());
                    continue;
                }
            };
            if !opened.cache_hit {
                summary.scanned_bytes = summary
                    .scanned_bytes
                    .saturating_add(opened.reader.byte_len());
            }
            // 分页文档必须批量合并连续字节读取，避免搜索退化为每行一次 seek/read。
            let traversal =
                opened
                    .reader
                    .for_each_line(cancel_flag.as_ref(), |line_number, line_text| {
                        summary.scanned_lines = summary.scanned_lines.saturating_add(1);
                        let matched_pattern_ids = compiled
                            .iter()
                            .filter(|pattern| pattern.matcher.is_match(line_text))
                            .map(|pattern| pattern.pattern_id.clone())
                            .collect::<Vec<_>>();
                        if matched_pattern_ids.is_empty() {
                            return true;
                        }
                        summary.matched_results = summary.matched_results.saturating_add(1);
                        hit_callback(AgentSearchHit {
                            source_ref: source.source_ref.clone(),
                            relative_path: source.relative_path.clone(),
                            line_number,
                            line_text: line_text.to_string(),
                            matched_pattern_ids,
                        });
                        true
                    });
            if let Err(error) = traversal {
                if cancel_flag.load(Ordering::Relaxed) {
                    summary.was_cancelled = true;
                    break;
                }
                summary.errors.push(error.to_string());
            }
            if summary.was_cancelled {
                break;
            }
        }
        summary
    }
}

/// 把普通关键字转义为正则，把显式正则原样编译，并统一应用大小写设置。
fn compile_pattern(pattern: &AgentSearchPattern) -> Result<CompiledAgentSearchPattern, String> {
    if pattern.query.is_empty() {
        return Err("搜索表达式不能为空".to_string());
    }
    let expression = if pattern.regex {
        pattern.query.clone()
    } else {
        regex::escape(&pattern.query)
    };
    let matcher = RegexBuilder::new(&expression)
        .case_insensitive(!pattern.case_sensitive)
        .build()
        .map_err(|error| format!("搜索表达式无效：{error}"))?;
    Ok(CompiledAgentSearchPattern {
        pattern_id: pattern.pattern_id.clone(),
        matcher,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证无效正则在读取来源前即可拒绝。
    #[test]
    fn invalid_regex_is_rejected_without_log_access() {
        let error = AgentLogSearchEngine::validate_pattern(&AgentSearchPattern {
            pattern_id: "invalid".to_string(),
            query: "[".to_string(),
            case_sensitive: false,
            regex: true,
        })
        .expect_err("无效正则必须被拒绝");

        assert!(error.contains("搜索表达式无效"));
    }
}
