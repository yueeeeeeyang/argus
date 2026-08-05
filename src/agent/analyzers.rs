//! 文件职责：实现仅供 AI Agent 使用的确定性 JStack 与 Runtime 日志分析器。
//! 创建日期：2026-07-17
//! 修改日期：2026-07-17
//! 作者：Argus 开发团队
//! 主要功能：基于 Agent 原生日志文档聚合线程状态、Runtime 请求耗时和 SQL 线索，不调用界面分析模块。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use serde_json::{Value, json};

use crate::agent::log_access::AgentLogAccess;
use crate::agent::session::SnapshotSource;

/// Agent 专项分析器返回的本地制品和安全摘要。
#[derive(Debug)]
pub(crate) struct AgentAnalyzerResult {
    /// 可持久到会话制品的结构化 JSON。
    pub artifact: Value,
    /// 直接返回模型的短摘要。
    pub summary: String,
    /// 本次首次打开日志的真实字节数。
    pub scanned_bytes: u64,
}

/// 不依赖主窗口分析页面的 Agent 专用分析器集合。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AgentNativeAnalyzers;

impl AgentNativeAnalyzers {
    /// 聚合线程快照中的线程名称和 Java 状态。
    pub(crate) fn analyze_jstack(
        access: &AgentLogAccess,
        sources: &[SnapshotSource],
        cancel_flag: Arc<AtomicBool>,
    ) -> AgentAnalyzerResult {
        let mut thread_counts = BTreeMap::<String, usize>::new();
        let mut state_counts = BTreeMap::<String, usize>::new();
        let mut snapshot_count = 0_usize;
        let mut total_samples = 0_usize;
        let mut scanned_bytes = 0_u64;
        let mut skipped = Vec::new();

        for source in sources {
            if cancel_flag.load(Ordering::Relaxed) {
                break;
            }
            let opened = match access.open(&source.source_ref, cancel_flag.clone()) {
                Ok(opened) => opened,
                Err(_) => {
                    skipped.push(json!({
                        "label": source.file_name,
                        "reason": "Agent 专用日志读取失败",
                    }));
                    continue;
                }
            };
            if !opened.cache_hit {
                scanned_bytes = scanned_bytes.saturating_add(opened.reader.byte_len());
            }
            let mut file_thread_counts = BTreeMap::<String, usize>::new();
            let mut file_state_counts = BTreeMap::<String, usize>::new();
            let mut file_threads = 0_usize;
            let mut current_thread_seen = false;
            // 分页日志按连续字节批量遍历，避免专项分析器对大文件逐行随机读取。
            let traversal =
                opened
                    .reader
                    .for_each_line(cancel_flag.as_ref(), |_line_number, line| {
                        if let Some(thread_name) = parse_jstack_thread_name(line) {
                            *file_thread_counts.entry(thread_name).or_default() += 1;
                            file_threads = file_threads.saturating_add(1);
                            current_thread_seen = true;
                            return true;
                        }
                        if current_thread_seen && let Some(state) = parse_jstack_state(line) {
                            *file_state_counts.entry(state).or_default() += 1;
                            current_thread_seen = false;
                        }
                        true
                    });
            if traversal.is_err() {
                if cancel_flag.load(Ordering::Relaxed) {
                    break;
                }
                skipped.push(json!({
                    "label": source.file_name,
                    "reason": "Agent 专用日志遍历失败",
                }));
                continue;
            }
            // 单文件完整遍历后再提交统计，防止 I/O 中途失败留下半份样本。
            for (thread_name, count) in file_thread_counts {
                *thread_counts.entry(thread_name).or_default() += count;
            }
            for (state, count) in file_state_counts {
                *state_counts.entry(state).or_default() += count;
            }
            if file_threads > 0 {
                snapshot_count = snapshot_count.saturating_add(1);
                total_samples = total_samples.saturating_add(file_threads);
            } else {
                skipped.push(json!({
                    "label": source.file_name,
                    "reason": "未识别到 JStack 线程头",
                }));
            }
        }

        let mut top_threads = thread_counts
            .iter()
            .map(|(thread, count)| json!({ "thread": thread, "total_count": count }))
            .collect::<Vec<_>>();
        top_threads.sort_by(|left, right| {
            right["total_count"]
                .as_u64()
                .cmp(&left["total_count"].as_u64())
                .then_with(|| left["thread"].as_str().cmp(&right["thread"].as_str()))
        });
        top_threads.truncate(50);
        let thread_count = thread_counts.len();
        let skipped_count = skipped.len();
        skipped.truncate(20);
        AgentAnalyzerResult {
            artifact: json!({
                "analyzer": "jstack_state_summary",
                "snapshot_count": snapshot_count,
                "thread_count": thread_count,
                "total_samples": total_samples,
                "state_counts": state_counts,
                "skipped_count": skipped_count,
                "top_threads": top_threads,
                "skipped": skipped,
            }),
            summary: format!(
                "Jstack：解析 {snapshot_count} 个快照、{thread_count} 个线程、{total_samples} 个样本，跳过 {skipped_count} 个文件"
            ),
            scanned_bytes,
        }
    }

    /// 按 Runtime 六段文件名和五耗时 SQL 行格式聚合请求与慢 SQL 线索。
    pub(crate) fn analyze_runtime(
        access: &AgentLogAccess,
        sources: &[SnapshotSource],
        cancel_flag: Arc<AtomicBool>,
    ) -> AgentAnalyzerResult {
        let mut groups = BTreeMap::<String, RuntimeGroup>::new();
        let mut request_count = 0_usize;
        let mut total_sql_records = 0_usize;
        let mut scanned_bytes = 0_u64;
        let mut skipped = Vec::new();

        for source in sources {
            if cancel_flag.load(Ordering::Relaxed) {
                break;
            }
            let Some(metadata) = parse_runtime_file_name(&source.file_name) else {
                skipped.push(json!({
                    "label": source.file_name,
                    "reason": "文件名不符合 Runtime 六段格式",
                }));
                continue;
            };
            let opened = match access.open(&source.source_ref, cancel_flag.clone()) {
                Ok(opened) => opened,
                Err(_) => {
                    skipped.push(json!({
                        "label": source.file_name,
                        "reason": "Agent 专用日志读取失败",
                    }));
                    continue;
                }
            };
            if !opened.cache_hit {
                scanned_bytes = scanned_bytes.saturating_add(opened.reader.byte_len());
            }
            let mut sql_count = 0_usize;
            let mut sql_execute_ms = 0_u64;
            // 复用分页读取器的顺序批量 I/O，保证大型非 UTF-8 Runtime 日志仍可有界扫描。
            let traversal =
                opened
                    .reader
                    .for_each_line(cancel_flag.as_ref(), |_line_number, line| {
                        if let Some(execute_ms) = parse_runtime_sql_execute_ms(line) {
                            sql_count = sql_count.saturating_add(1);
                            sql_execute_ms = sql_execute_ms.saturating_add(execute_ms);
                        }
                        true
                    });
            if traversal.is_err() {
                if cancel_flag.load(Ordering::Relaxed) {
                    break;
                }
                skipped.push(json!({
                    "label": source.file_name,
                    "reason": "Agent 专用日志遍历失败",
                }));
                continue;
            }
            total_sql_records = total_sql_records.saturating_add(sql_count);
            request_count = request_count.saturating_add(1);
            let is_slow = metadata.duration_ms > 0
                && sql_execute_ms.saturating_mul(100) > metadata.duration_ms.saturating_mul(90);
            let group = groups.entry(metadata.request_path).or_default();
            group.request_count = group.request_count.saturating_add(1);
            group.total_duration_ms = group.total_duration_ms.saturating_add(metadata.duration_ms);
            group.slow_request_count = group
                .slow_request_count
                .saturating_add(usize::from(is_slow));
        }

        let mut top_requests = groups
            .into_iter()
            .map(|(request_path, group)| {
                let average_duration_ms = if group.request_count == 0 {
                    0.0
                } else {
                    group.total_duration_ms as f64 / group.request_count as f64
                };
                let slow_sql_ratio = if group.request_count == 0 {
                    0.0
                } else {
                    group.slow_request_count as f64 / group.request_count as f64
                };
                json!({
                    "request_path": request_path,
                    "request_count": group.request_count,
                    "average_duration_ms": average_duration_ms,
                    "slow_request_count": group.slow_request_count,
                    "slow_sql_ratio": slow_sql_ratio,
                })
            })
            .collect::<Vec<_>>();
        top_requests.sort_by(|left, right| {
            right["request_count"]
                .as_u64()
                .cmp(&left["request_count"].as_u64())
                .then_with(|| {
                    left["request_path"]
                        .as_str()
                        .cmp(&right["request_path"].as_str())
                })
        });
        // 完整分组数量必须在裁剪模型可见明细前保存，避免地址超过 100 个时低估聚合覆盖面。
        let summary_count = top_requests.len();
        top_requests.truncate(100);
        let skipped_count = skipped.len();
        skipped.truncate(20);
        AgentAnalyzerResult {
            artifact: json!({
                "analyzer": "runtime_error_summary",
                "total_files": sources.len(),
                "request_count": request_count,
                "summary_count": summary_count,
                "total_sql_records": total_sql_records,
                "skipped_count": skipped_count,
                "top_requests": top_requests,
                "skipped": skipped,
            }),
            summary: format!(
                "Runtime：解析 {request_count} 个请求、{summary_count} 个地址和 {total_sql_records} 条 SQL，跳过 {skipped_count} 个文件"
            ),
            scanned_bytes,
        }
    }
}

/// Runtime 请求地址的累计状态。
#[derive(Debug, Default)]
struct RuntimeGroup {
    /// 请求数。
    request_count: usize,
    /// 请求总耗时。
    total_duration_ms: u64,
    /// SQL 耗时超过请求 90% 的请求数。
    slow_request_count: usize,
}

/// Runtime 文件名中当前分析需要的最小元数据。
struct RuntimeMetadata {
    /// 请求总耗时。
    duration_ms: u64,
    /// 把 `_` 转为 `/` 后的请求地址。
    request_path: String,
}

/// 解析 JStack 线程头的首个双引号字段。
fn parse_jstack_thread_name(line: &str) -> Option<String> {
    let rest = line.strip_prefix('"')?;
    let end = rest.find('"')?;
    let name = rest[..end].trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// 解析 `java.lang.Thread.State:` 后的状态词。
fn parse_jstack_state(line: &str) -> Option<String> {
    let (_, state) = line.split_once("java.lang.Thread.State:")?;
    state
        .split_whitespace()
        .next()
        .filter(|state| !state.is_empty())
        .map(str::to_string)
}

/// 解析 `耗时&用户&API&时间戳&socket&安全.log` 六段 Runtime 文件名。
fn parse_runtime_file_name(file_name: &str) -> Option<RuntimeMetadata> {
    let stem = Path::new(file_name).file_stem()?.to_str()?;
    let parts = stem.split('&').collect::<Vec<_>>();
    if parts.len() != 6 {
        return None;
    }
    let duration_ms = parts[0].parse::<u64>().ok()?;
    parts[3].parse::<i64>().ok()?;
    parts[4].parse::<u64>().ok()?;
    parts[5].parse::<u64>().ok()?;
    let request_path = parts[2].replace('_', "/");
    (!request_path.is_empty()).then_some(RuntimeMetadata {
        duration_ms,
        request_path,
    })
}

/// 识别由五个耗时 token 开头的 Runtime SQL 记录，并返回 SQL 执行耗时。
fn parse_runtime_sql_execute_ms(line: &str) -> Option<u64> {
    let mut tokens = line.split_whitespace();
    let execute_ms = parse_duration_token(tokens.next()?)?;
    for _ in 0..4 {
        parse_duration_token(tokens.next()?)?;
    }
    tokens.next()?;
    Some(execute_ms)
}

/// 解析 `12ms` 或裸数字毫秒 token。
fn parse_duration_token(token: &str) -> Option<u64> {
    token.strip_suffix("ms").unwrap_or(token).parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 JStack 线程头和状态解析仅接受明确格式。
    #[test]
    fn parses_jstack_thread_and_state() {
        assert_eq!(
            parse_jstack_thread_name("\"main\" #1"),
            Some("main".to_string())
        );
        assert_eq!(
            parse_jstack_state("   java.lang.Thread.State: RUNNABLE"),
            Some("RUNNABLE".to_string())
        );
    }

    /// 验证 Runtime 文件名和 SQL 行只提取 Agent 聚合所需字段。
    #[test]
    fn parses_runtime_minimum_fields() {
        let metadata = parse_runtime_file_name("100&user&_api_demo&1782368843095&0&0.log")
            .expect("Runtime 文件名应解析成功");
        assert_eq!(metadata.duration_ms, 100);
        assert_eq!(metadata.request_path, "/api/demo");
        assert_eq!(
            parse_runtime_sql_execute_ms("91ms 0ms 0ms 0ms 0ms select 1"),
            Some(91)
        );
    }
}
