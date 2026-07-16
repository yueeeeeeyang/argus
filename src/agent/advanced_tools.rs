//! 文件职责：实现 Argus Agent 的高级来源概览、批量检索、采样、事件聚合和制品查询工具。
//! 创建日期：2026-07-16
//! 修改日期：2026-07-16
//! 作者：Argus 开发团队
//! 主要功能：提供只接受声明式参数的 P0/P1 日志分析能力，并复用现有读取、预算、取消、脱敏和证据边界。

use std::cmp::Ordering as CmpOrdering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};

use chrono::{DateTime, NaiveDateTime, Utc};
use regex::Regex;
use rig_core::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::agent::session::{
    AgentOperationContext, MAX_TOOL_RAW_BYTES, SnapshotSource, truncate_utf8_with_ellipsis,
};
use crate::agent::tools::{
    AgentToolError, checked_output, estimated_full_scan_bytes, reconcile_tool_scan,
    redact_error_path, redact_sensitive_text, selected_sources, source_ref_by_id,
    validate_tool_output_size,
};
use crate::reader::log_file_reader::{LogFileReader, LogReaderHandle, OpenLogRequest};
use crate::search::search_engine::{SearchEngine, SearchQuery, SearchRequest, SearchTarget};

/// 批量搜索允许的最大模式数量；单次扫描复用现有多查询搜索引擎。
const MAX_BATCH_SEARCH_PATTERNS: usize = 20;
/// 来源概览单页最多返回的日志类型数量。
const MAX_OVERVIEW_PROFILE_PAGE: usize = 20;
/// 事件聚合最多维护的不同签名数量，防止高基数字段耗尽内存。
const MAX_EVENT_SIGNATURE_GROUPS: usize = 10_000;
/// 通用日志扫描每批读取的行数，在批次边界响应取消。
const ADVANCED_SCAN_BATCH_LINES: usize = 4096;

/// 把会话取消令牌桥接到阻塞日志读取循环。
struct BlockingCancellationGuard {
    /// 阻塞代码在来源、归档块和行批次边界读取该标记。
    cancel_flag: Arc<AtomicBool>,
    /// 异步等待会话取消的桥接任务。
    watcher: tokio::task::JoinHandle<()>,
}

impl BlockingCancellationGuard {
    /// 为当前会话建立原子取消标记和自动清理守卫。
    fn new(context: &AgentOperationContext) -> (Arc<AtomicBool>, Self) {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let watcher_flag = cancel_flag.clone();
        let cancellation = context.cancellation.clone();
        let watcher = tokio::spawn(async move {
            cancellation.cancelled().await;
            watcher_flag.store(true, Ordering::Relaxed);
        });
        (
            cancel_flag.clone(),
            Self {
                cancel_flag,
                watcher,
            },
        )
    }
}

impl Drop for BlockingCancellationGuard {
    /// 工具 future 被丢弃时也通知阻塞任务停止，随后终止桥接任务。
    fn drop(&mut self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
        self.watcher.abort();
    }
}

/// 来源概览分页参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct GetSourceOverviewArgs {
    /// 日志类型分页偏移。
    #[serde(default)]
    pub profile_offset: usize,
    /// 单页日志类型数量，范围 1～20。
    #[serde(default = "default_overview_profile_limit")]
    pub profile_limit: usize,
    /// 最多返回的扩展名分组数，范围 1～50。
    #[serde(default = "default_extension_limit")]
    pub extension_limit: usize,
}

/// 来源概览中的扩展名统计。
#[derive(Debug, Serialize)]
struct ExtensionCountOutput {
    /// 小写扩展名；无扩展名时为 `(none)`。
    extension: String,
    /// 当前扩展名的文件数。
    file_count: usize,
}

/// 来源概览中的大文件摘要。
#[derive(Debug, Serialize)]
struct LargestSourceOutput {
    /// 会话不透明来源引用。
    source_ref: String,
    /// 来源根内相对展示路径。
    relative_path: String,
    /// 已知文件或解压条目大小。
    size: u64,
}

/// 一条日志名称规则的命中统计。
#[derive(Debug, Serialize)]
struct MatcherOverviewOutput {
    /// 匹配目标的配置枚举名称。
    target: crate::config::LogNameMatcherTarget,
    /// 匹配算法的配置枚举名称。
    mode: crate::config::LogNameMatcherMode,
    /// 有界后的规则模式。
    pattern: String,
    /// 是否区分大小写。
    case_sensitive: bool,
    /// 当前范围内命中该规则的来源数量。
    matched_file_count: usize,
}

/// 一个日志类型在当前来源范围中的摘要。
#[derive(Debug, Serialize)]
struct ProfileOverviewOutput {
    /// 稳定日志类型配置 ID。
    profile_id: String,
    /// 用户可读类型名称。
    name: String,
    /// 跨类型决胜优先级。
    priority: u16,
    /// 任意规则命中的文件数，不考虑其它类型优先级。
    matched_file_count: usize,
    /// 最终采用该类型的文件数。
    selected_file_count: usize,
    /// 最终采用来源的少量不透明引用样例。
    source_refs: Vec<String>,
    /// 当前类型的逐规则命中统计。
    rules: Vec<MatcherOverviewOutput>,
}

/// 来源范围总体概览输出。
#[derive(Debug, Serialize)]
pub(crate) struct GetSourceOverviewOutput {
    /// 来源根展示名称。
    root_label: String,
    /// 可读取日志来源总数。
    total_sources: usize,
    /// 具有已知大小的来源累计字节数。
    known_total_bytes: u64,
    /// 大小元数据缺失的来源数量。
    unknown_size_sources: usize,
    /// 未最终匹配任何日志类型说明的来源数量。
    unmatched_sources: usize,
    /// 文件扩展名 Top-N 分布。
    extensions: Vec<ExtensionCountOutput>,
    /// 已知大小最大的十个来源。
    largest_sources: Vec<LargestSourceOutput>,
    /// 当前日志类型分页。
    profiles: Vec<ProfileOverviewOutput>,
    /// 下一页日志类型偏移。
    next_profile_offset: Option<usize>,
}

/// 基于会话快照生成来源地图，不读取任何日志正文。
#[derive(Clone)]
pub(crate) struct GetSourceOverviewTool(pub Arc<AgentOperationContext>);

impl Tool for GetSourceOverviewTool {
    const NAME: &'static str = "get_source_overview";
    type Error = AgentToolError;
    type Args = GetSourceOverviewArgs;
    type Output = GetSourceOverviewOutput;

    fn description(&self) -> String {
        "汇总来源数量、大小、扩展名和日志类型逐规则命中数；只读取会话元数据，不返回日志正文。"
            .to_string()
    }

    fn parameters(&self) -> Value {
        schema_value::<Self::Args>()
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        self.0
            .begin_tool(Self::NAME, 0)
            .map_err(AgentToolError::new)?;
        let mut extension_counts = BTreeMap::<String, usize>::new();
        let mut known_total_bytes = 0_u64;
        let mut unknown_size_sources = 0_usize;
        let mut largest_sources = self
            .0
            .scope
            .sources
            .iter()
            .filter_map(|source| {
                let extension = source_extension(&source.file_name);
                *extension_counts.entry(extension).or_default() += 1;
                match source.size {
                    Some(size) => known_total_bytes = known_total_bytes.saturating_add(size),
                    None => unknown_size_sources += 1,
                }
                source.size.map(|size| LargestSourceOutput {
                    source_ref: source.source_ref.clone(),
                    relative_path: source.relative_path.clone(),
                    size,
                })
            })
            .collect::<Vec<_>>();
        largest_sources.sort_by_key(|source| std::cmp::Reverse(source.size));
        largest_sources.truncate(10);

        let extension_limit = args.extension_limit.clamp(1, 50);
        let mut extensions = extension_counts
            .into_iter()
            .map(|(extension, file_count)| ExtensionCountOutput {
                extension,
                file_count,
            })
            .collect::<Vec<_>>();
        extensions.sort_by(|left, right| {
            right
                .file_count
                .cmp(&left.file_count)
                .then_with(|| left.extension.cmp(&right.extension))
        });
        extensions.truncate(extension_limit);

        let mut profiles = self.0.scope.profiles.values().collect::<Vec<_>>();
        profiles.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.profile_id.cmp(&right.profile_id))
        });
        let limit = args.profile_limit.clamp(1, MAX_OVERVIEW_PROFILE_PAGE);
        let end = args
            .profile_offset
            .saturating_add(limit)
            .min(profiles.len());
        let profile_outputs = profiles
            .get(args.profile_offset..end)
            .unwrap_or_default()
            .iter()
            .map(|profile| profile_overview(profile, &self.0.scope.sources))
            .collect::<Vec<_>>();
        let unmatched_sources = self
            .0
            .scope
            .sources
            .iter()
            .filter(|source| source.profile_id.is_none())
            .count();

        checked_output(GetSourceOverviewOutput {
            root_label: self.0.scope.root_label.clone(),
            total_sources: self.0.scope.sources.len(),
            known_total_bytes,
            unknown_size_sources,
            unmatched_sources,
            extensions,
            largest_sources,
            profiles: profile_outputs,
            next_profile_offset: (end < profiles.len()).then_some(end),
        })
    }
}

/// 根据会话日志配置快照计算一个类型的规则与最终选择统计。
fn profile_overview(
    profile: &crate::agent::session::LogProfileSnapshot,
    sources: &[SnapshotSource],
) -> ProfileOverviewOutput {
    let mut rule_counts = vec![0_usize; profile.matchers.len()];
    let mut matched_file_count = 0_usize;
    let mut selected_file_count = 0_usize;
    let mut source_refs = Vec::new();
    for source in sources {
        let mut matched = false;
        for (index, matcher) in profile.matchers.iter().enumerate() {
            if matcher.is_match(&source.file_name, &source.relative_path) {
                rule_counts[index] += 1;
                matched = true;
            }
        }
        if matched {
            matched_file_count += 1;
        }
        if source.profile_id.as_deref() == Some(profile.profile_id.as_str()) {
            selected_file_count += 1;
            if source_refs.len() < 20 {
                source_refs.push(source.source_ref.clone());
            }
        }
    }
    let rules = profile
        .matchers
        .iter()
        .zip(rule_counts)
        .map(|(matcher, matched_file_count)| MatcherOverviewOutput {
            target: matcher.target,
            mode: matcher.mode,
            pattern: truncate_utf8_with_ellipsis(matcher.pattern.clone(), 128),
            case_sensitive: matcher.case_sensitive,
            matched_file_count,
        })
        .collect();
    ProfileOverviewOutput {
        profile_id: profile.profile_id.clone(),
        name: profile.name.clone(),
        priority: profile.priority,
        matched_file_count,
        selected_file_count,
        source_refs,
        rules,
    }
}

/// 返回来源文件名的小写扩展名，没有扩展名时使用稳定占位符。
fn source_extension(file_name: &str) -> String {
    Path::new(file_name)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty())
        .map(|extension| format!(".{}", extension.to_ascii_lowercase()))
        .unwrap_or_else(|| "(none)".to_string())
}

/// 一个批量搜索模式。
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub(crate) struct BatchSearchPatternArgs {
    /// 模型定义的稳定短标识，用于关联结果。
    pub pattern_id: String,
    /// 搜索文本或 Rust 正则。
    pub query: String,
    /// 是否按 Rust 正则解释查询。
    #[serde(default)]
    pub regex: bool,
    /// 是否区分大小写。
    #[serde(default)]
    pub case_sensitive: bool,
}

/// 批量搜索参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct SearchLogsBatchArgs {
    /// 1～20 个查询模式；query 必须互不相同。
    pub patterns: Vec<BatchSearchPatternArgs>,
    /// 来源引用；为空时扫描全部授权来源。
    #[serde(default)]
    pub source_refs: Vec<String>,
    /// 每个模式最多返回的代表性命中行数，范围 1～20。
    #[serde(default = "default_batch_results_per_pattern")]
    pub max_results_per_pattern: usize,
}

/// 批量搜索中的代表性命中行。
#[derive(Clone, Debug, Serialize)]
struct BatchSearchHitOutput {
    /// 不透明来源引用。
    source_ref: String,
    /// 来源根内相对展示路径。
    relative_path: String,
    /// 1 基行号。
    line: usize,
    /// 授权后返回的脱敏、有界正文。
    text: Option<String>,
}

/// 一个批量模式的搜索结果。
#[derive(Debug, Serialize)]
struct BatchPatternResultOutput {
    /// 输入模式标识。
    pattern_id: String,
    /// 当前模式命中的日志行数。
    matched_lines: usize,
    /// 代表性命中行。
    hits: Vec<BatchSearchHitOutput>,
    /// 是否还有未返回的命中行。
    truncated: bool,
}

/// 批量搜索输出。
#[derive(Debug, Serialize)]
pub(crate) struct SearchLogsBatchOutput {
    /// 按输入顺序返回的模式结果。
    patterns: Vec<BatchPatternResultOutput>,
    /// 实际扫描文件数。
    scanned_files: usize,
    /// 实际扫描行数。
    scanned_lines: usize,
    /// 已脱敏的非致命读取错误。
    errors: Vec<String>,
    /// 是否还有未返回的来源读取错误。
    errors_truncated: bool,
}

/// 单次扫描同时执行多个关键字或正则，并为每个模式保留少量代表性证据。
#[derive(Clone)]
pub(crate) struct SearchLogsBatchTool(pub Arc<AgentOperationContext>);

impl Tool for SearchLogsBatchTool {
    const NAME: &'static str = "search_logs_batch";
    type Error = AgentToolError;
    type Args = SearchLogsBatchArgs;
    type Output = SearchLogsBatchOutput;

    fn description(&self) -> String {
        "一次扫描执行 1～20 个关键字或 Rust 正则，分别返回命中行数和少量代表性证据。".to_string()
    }

    fn parameters(&self) -> Value {
        schema_value::<Self::Args>()
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        validate_batch_patterns(&args.patterns)?;
        let selected = selected_sources(&self.0, &args.source_refs)?;
        let scan_bytes = estimated_full_scan_bytes(&selected);
        self.0
            .begin_tool(Self::NAME, scan_bytes)
            .map_err(AgentToolError::new)?;
        let max_results = args.max_results_per_pattern.clamp(1, 20);
        let queries = args
            .patterns
            .iter()
            .map(|pattern| SearchQuery {
                keyword: pattern.query.clone(),
                case_sensitive: pattern.case_sensitive,
                regex_enabled: pattern.regex,
            })
            .collect::<Vec<_>>();
        let request = SearchRequest::with_queries(
            queries,
            selected
                .iter()
                .map(|source| SearchTarget {
                    source_id: source.source_id,
                    label: source.file_name.clone(),
                    path: source.relative_path.clone(),
                    location: source.location.clone(),
                })
                .collect(),
            self.0.scope.default_encoding.clone(),
        )
        .with_archive_passwords(self.0.scope.archive_passwords.clone());
        let query_indices = args
            .patterns
            .iter()
            .enumerate()
            .map(|(index, pattern)| (pattern.query.clone(), index))
            .collect::<HashMap<_, _>>();
        let source_refs = source_ref_by_id(&self.0);
        let accumulator = Arc::new(Mutex::new(BatchSearchAccumulator {
            matched_lines: vec![0; args.patterns.len()],
            hits: vec![Vec::new(); args.patterns.len()],
            raw_bytes: 0,
        }));
        let callback_accumulator = accumulator.clone();
        let allow_raw = self.0.scope.allow_raw_log_content;
        let (cancel_flag, _cancel_guard) = BlockingCancellationGuard::new(&self.0);
        let summary = tokio::task::spawn_blocking(move || {
            SearchEngine::search(
                request,
                |_| {},
                move |batch| {
                    let Ok(mut accumulator) = callback_accumulator.lock() else {
                        return;
                    };
                    for result in batch {
                        for keyword in &result.matched_keywords {
                            let Some(index) = query_indices.get(keyword).copied() else {
                                continue;
                            };
                            accumulator.matched_lines[index] =
                                accumulator.matched_lines[index].saturating_add(1);
                            if accumulator.hits[index].len() >= max_results {
                                continue;
                            }
                            let text = if allow_raw {
                                // 按最终模型可见字节增量裁剪，合法的最大批量参数也必须返回部分结果而不是事后整体失败。
                                let remaining = MAX_TOOL_RAW_BYTES
                                    .saturating_sub(accumulator.raw_bytes)
                                    .min(4096);
                                if remaining == 0 {
                                    continue;
                                }
                                let (text, _) = truncate_text_to_total_bytes(
                                    redact_sensitive_text(&result.line_text),
                                    remaining,
                                );
                                if text.is_empty() && !result.line_text.is_empty() {
                                    continue;
                                }
                                accumulator.raw_bytes =
                                    accumulator.raw_bytes.saturating_add(text.len());
                                Some(text)
                            } else {
                                None
                            };
                            accumulator.hits[index].push(BatchSearchHitOutput {
                                source_ref: source_refs
                                    .get(&result.source_id.0)
                                    .cloned()
                                    .unwrap_or_default(),
                                relative_path: result.path.clone(),
                                line: result.line_number + 1,
                                text,
                            });
                        }
                    }
                },
                cancel_flag,
            )
        })
        .await
        .map_err(|error| AgentToolError::new(format!("批量日志搜索任务异常结束：{error}")))?;
        reconcile_tool_scan(&self.0, scan_bytes, summary.scanned_bytes)?;
        let accumulator = Arc::try_unwrap(accumulator)
            .map_err(|_| AgentToolError::new("批量搜索结果仍被后台任务占用"))?
            .into_inner()
            .map_err(|_| AgentToolError::new("批量搜索结果状态已损坏"))?;
        let patterns = args
            .patterns
            .into_iter()
            .enumerate()
            .map(|(index, pattern)| BatchPatternResultOutput {
                pattern_id: pattern.pattern_id,
                matched_lines: accumulator.matched_lines[index],
                truncated: accumulator.matched_lines[index] > accumulator.hits[index].len(),
                hits: accumulator.hits[index].clone(),
            })
            .collect();
        let error_count = summary.errors.len();
        let errors = summary
            .errors
            .into_iter()
            .take(20)
            .map(redact_error_path)
            .collect();
        let mut output = SearchLogsBatchOutput {
            patterns,
            scanned_files: summary.scanned_files,
            scanned_lines: summary.scanned_lines,
            errors,
            errors_truncated: error_count > 20,
        };
        trim_batch_search_output(&mut output)?;
        let raw_bytes = output
            .patterns
            .iter()
            .flat_map(|pattern| &pattern.hits)
            .filter_map(|hit| hit.text.as_ref())
            .map(String::len)
            .sum::<usize>();
        if raw_bytes > 0 {
            self.0
                .budget
                .consume_raw_log_bytes(raw_bytes)
                .map_err(AgentToolError::new)?;
            self.0.publish_budget();
        }
        for hit in output.patterns.iter().flat_map(|pattern| &pattern.hits) {
            self.0
                .evidence_ranges
                .record(&hit.source_ref, hit.line, hit.line)
                .map_err(AgentToolError::new)?;
        }
        checked_output(output)
    }
}

/// 阻塞批量搜索使用的有界累计状态。
struct BatchSearchAccumulator {
    /// 每个模式命中的行数。
    matched_lines: Vec<usize>,
    /// 每个模式保留的代表性命中。
    hits: Vec<Vec<BatchSearchHitOutput>>,
    /// 已保留模型可见原文的累计 UTF-8 字节数。
    raw_bytes: usize,
}

/// 在保留每个模式计数的前提下裁剪代表性命中，确保最终 JSON 不会因合法参数整体失败。
fn trim_batch_search_output(output: &mut SearchLogsBatchOutput) -> Result<(), AgentToolError> {
    while validate_tool_output_size(output).is_err() {
        let Some(pattern) = output
            .patterns
            .iter_mut()
            .filter(|pattern| !pattern.hits.is_empty())
            .max_by_key(|pattern| pattern.hits.len())
        else {
            return Err(AgentToolError::new(
                "批量搜索聚合结果超过 128 KiB，且已无代表性命中可继续裁剪",
            ));
        };
        pattern.hits.pop();
        pattern.truncated = true;
    }
    Ok(())
}

/// 校验批量搜索模式数量、标识和查询唯一性。
fn validate_batch_patterns(patterns: &[BatchSearchPatternArgs]) -> Result<(), AgentToolError> {
    if !(1..=MAX_BATCH_SEARCH_PATTERNS).contains(&patterns.len()) {
        return Err(AgentToolError::new("patterns 必须包含 1～20 个搜索模式"));
    }
    let mut ids = BTreeSet::new();
    let mut queries = BTreeSet::new();
    for pattern in patterns {
        if pattern.pattern_id.is_empty()
            || pattern.pattern_id.len() > 64
            || !pattern
                .pattern_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(AgentToolError::new(
                "pattern_id 必须是长度 1～64 的字母、数字、下划线或连字符",
            ));
        }
        if pattern.query.trim().is_empty() || pattern.query.len() > 512 {
            return Err(AgentToolError::new("批量搜索表达式长度必须为 1～512 B"));
        }
        if !ids.insert(pattern.pattern_id.as_str()) {
            return Err(AgentToolError::new("pattern_id 不能重复"));
        }
        if !queries.insert(pattern.query.as_str()) {
            return Err(AgentToolError::new(
                "批量搜索 query 不能重复，请合并相同查询",
            ));
        }
        // 在预留全量扫描预算前复用现有搜索引擎编译正则，避免无效表达式产生虚假的扫描计量。
        SearchEngine::validate_query(&SearchQuery {
            keyword: pattern.query.clone(),
            case_sensitive: pattern.case_sensitive,
            regex_enabled: pattern.regex,
        })
        .map_err(|error| {
            AgentToolError::new(format!("批量搜索模式“{}”无效：{error}", pattern.pattern_id))
        })?;
    }
    Ok(())
}

/// 有界日志采样策略。
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LogSampleStrategy {
    /// 从文件开头采样。
    Head,
    /// 从文件末尾采样。
    Tail,
    /// 在完整行范围内均匀采样。
    Uniform,
}

/// 有界日志采样参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct SampleLogArgs {
    /// 单个会话来源引用。
    pub source_ref: String,
    /// head、tail 或 uniform。
    pub strategy: LogSampleStrategy,
    /// 最多返回行数，范围 1～60。
    #[serde(default = "default_sample_lines")]
    pub max_lines: usize,
    /// 最多返回的脱敏正文 UTF-8 字节数，范围 1～32 KiB。
    #[serde(default = "default_sample_bytes")]
    pub max_bytes: usize,
}

/// 一条带真实行号的采样日志。
#[derive(Debug, Serialize)]
struct SampledLineOutput {
    /// 1 基行号。
    line: usize,
    /// 已脱敏且按字符边界裁剪的正文。
    text: String,
}

/// 有界日志采样输出。
#[derive(Debug, Serialize)]
pub(crate) struct SampleLogOutput {
    /// 来源引用。
    source_ref: String,
    /// 来源内相对路径。
    relative_path: String,
    /// 实际采样策略。
    strategy: LogSampleStrategy,
    /// 日志总行数。
    total_lines: usize,
    /// 实际返回的行。
    lines: Vec<SampledLineOutput>,
    /// 是否因为采样范围、行数或字节数省略了内容。
    truncated: bool,
}

/// 从单个授权来源的开头、末尾或均匀位置读取少量脱敏正文。
#[derive(Clone)]
pub(crate) struct SampleLogTool(pub Arc<AgentOperationContext>);

impl Tool for SampleLogTool {
    const NAME: &'static str = "sample_log";
    type Error = AgentToolError;
    type Args = SampleLogArgs;
    type Output = SampleLogOutput;

    fn description(&self) -> String {
        "按 head、tail 或 uniform 策略采样单个日志的少量行；需要原文授权，返回内容会脱敏。"
            .to_string()
    }

    fn parameters(&self) -> Value {
        schema_value::<Self::Args>()
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if !self.0.scope.allow_raw_log_content {
            return Err(AgentToolError::new("当前会话未授权向模型发送日志原文"));
        }
        let source = self
            .0
            .scope
            .source(&args.source_ref)
            .ok_or_else(|| AgentToolError::new("source_ref 不在当前会话范围内"))?
            .clone();
        let selected = vec![&source];
        let scan_bytes = estimated_full_scan_bytes(&selected);
        self.0
            .begin_tool(Self::NAME, scan_bytes)
            .map_err(AgentToolError::new)?;
        let max_lines = args.max_lines.clamp(1, 60);
        let max_bytes = args.max_bytes.clamp(1, 32 * 1024);
        let strategy = args.strategy;
        let scope = self.0.scope.clone();
        let (cancel_flag, _cancel_guard) = BlockingCancellationGuard::new(&self.0);
        let read = tokio::task::spawn_blocking(move || {
            let handle = LogFileReader::open_with_cancel_flag(
                OpenLogRequest {
                    location: source.location.clone(),
                    label: source.file_name.clone(),
                    default_encoding: scope.default_encoding.clone(),
                    archive_passwords: scope.archive_passwords.clone(),
                },
                cancel_flag.clone(),
            )?;
            let indices = sample_line_indices(handle.line_count(), max_lines, strategy);
            let mut lines = Vec::with_capacity(indices.len());
            for line_index in indices {
                if cancel_flag.load(Ordering::Relaxed) {
                    anyhow::bail!("日志采样已取消");
                }
                if let Some(line) = handle.lines(line_index, 1)?.into_iter().next() {
                    lines.push(line);
                }
            }
            anyhow::Ok((handle.byte_len(), handle.line_count(), lines))
        })
        .await
        .map_err(|error| AgentToolError::new(format!("日志采样任务异常结束：{error}")))?
        .map_err(|error| {
            AgentToolError::new(format!(
                "无法读取日志采样：{}",
                redact_error_path(error.to_string())
            ))
        })?;
        reconcile_tool_scan(&self.0, scan_bytes, read.0)?;
        let mut raw_bytes = 0_usize;
        let mut byte_truncated = false;
        let mut lines = Vec::new();
        for line in read.2 {
            let remaining = max_bytes.saturating_sub(raw_bytes);
            if remaining == 0 {
                byte_truncated = true;
                break;
            }
            let redacted = redact_sensitive_text(&line.text);
            let (text, was_truncated) = truncate_text_to_total_bytes(redacted, remaining);
            if text.is_empty() && !line.text.is_empty() {
                byte_truncated = true;
                break;
            }
            raw_bytes = raw_bytes.saturating_add(text.len());
            lines.push(SampledLineOutput {
                line: line.line_number + 1,
                text,
            });
            if was_truncated {
                byte_truncated = true;
                break;
            }
        }
        self.0
            .budget
            .consume_raw_log_bytes(raw_bytes)
            .map_err(AgentToolError::new)?;
        self.0.publish_budget();
        for line in &lines {
            self.0
                .evidence_ranges
                .record(&args.source_ref, line.line, line.line)
                .map_err(AgentToolError::new)?;
        }
        checked_output(SampleLogOutput {
            source_ref: args.source_ref,
            relative_path: source.relative_path,
            strategy,
            total_lines: read.1,
            truncated: byte_truncated || lines.len() < read.1,
            lines,
        })
    }
}

/// 计算指定策略需要读取的 0 基行号，并保持顺序和去重。
fn sample_line_indices(
    total_lines: usize,
    max_lines: usize,
    strategy: LogSampleStrategy,
) -> Vec<usize> {
    if total_lines == 0 {
        return Vec::new();
    }
    let count = total_lines.min(max_lines.max(1));
    match strategy {
        LogSampleStrategy::Head => (0..count).collect(),
        LogSampleStrategy::Tail => (total_lines - count..total_lines).collect(),
        LogSampleStrategy::Uniform if count == 1 => vec![0],
        LogSampleStrategy::Uniform => (0..count)
            .map(|index| index.saturating_mul(total_lines - 1) / (count - 1))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
    }
}

/// 按最终 UTF-8 总字节预算裁剪文本；省略号也计入预算。
fn truncate_text_to_total_bytes(value: String, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value, false);
    }
    if max_bytes < '…'.len_utf8() {
        return (String::new(), true);
    }
    (
        truncate_utf8_with_ellipsis(value, max_bytes - '…'.len_utf8()),
        true,
    )
}

/// 多行事件块提取参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ExtractEventBlocksArgs {
    /// 单个会话来源引用。
    pub source_ref: String,
    /// 事件内任意一行的 1 基行号。
    pub line: usize,
    /// 最多返回的事件块行数，范围 1～200。
    #[serde(default = "default_event_block_lines")]
    pub max_lines: usize,
    /// 最多返回的脱敏正文 UTF-8 字节数，范围 1～64 KiB。
    #[serde(default = "default_event_block_bytes")]
    pub max_bytes: usize,
}

/// 事件块中的一条日志行。
#[derive(Debug, Serialize)]
struct EventBlockLineOutput {
    /// 1 基行号。
    line: usize,
    /// 已脱敏正文。
    text: String,
}

/// 多行事件块输出。
#[derive(Debug, Serialize)]
pub(crate) struct ExtractEventBlocksOutput {
    /// 来源引用。
    source_ref: String,
    /// 事件块起始 1 基行号。
    start_line: usize,
    /// 事件块结束 1 基行号。
    end_line: usize,
    /// 归一化首行的短 SHA-256 指纹。
    fingerprint: String,
    /// 当前来源中相同归一化首行的出现次数。
    occurrence_count: usize,
    /// 最多二十个同指纹事件起始行。
    occurrence_lines: Vec<usize>,
    /// 实际返回的多行事件正文。
    lines: Vec<EventBlockLineOutput>,
    /// 是否因为行数或字节数截断事件块。
    truncated: bool,
    /// 是否因行数上限省略了真实事件首部。
    head_truncated: bool,
    /// 是否因行数上限省略了真实事件尾部。
    tail_truncated: bool,
    /// 去重口径说明。
    deduplication_basis: &'static str,
}

/// 提取异常堆栈等连续多行事件，并在同一来源内按归一化首行统计重复次数。
#[derive(Clone)]
pub(crate) struct ExtractEventBlocksTool(pub Arc<AgentOperationContext>);

impl Tool for ExtractEventBlocksTool {
    const NAME: &'static str = "extract_event_blocks";
    type Error = AgentToolError;
    type Args = ExtractEventBlocksArgs;
    type Output = ExtractEventBlocksOutput;

    fn description(&self) -> String {
        "根据来源和行号提取完整异常堆栈或连续事件块，并统计相同归一化首行的出现次数。".to_string()
    }

    fn parameters(&self) -> Value {
        schema_value::<Self::Args>()
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if !self.0.scope.allow_raw_log_content {
            return Err(AgentToolError::new("当前会话未授权向模型发送日志原文"));
        }
        if args.line == 0 {
            return Err(AgentToolError::new("事件块行号必须从 1 开始"));
        }
        let source = self
            .0
            .scope
            .source(&args.source_ref)
            .ok_or_else(|| AgentToolError::new("source_ref 不在当前会话范围内"))?
            .clone();
        let selected = vec![&source];
        let scan_bytes = estimated_full_scan_bytes(&selected);
        self.0
            .begin_tool(Self::NAME, scan_bytes)
            .map_err(AgentToolError::new)?;
        let max_lines = args.max_lines.clamp(1, 200);
        let max_bytes = args.max_bytes.clamp(1, MAX_TOOL_RAW_BYTES);
        let center = args.line - 1;
        let scope = self.0.scope.clone();
        let (cancel_flag, _cancel_guard) = BlockingCancellationGuard::new(&self.0);
        let extracted = tokio::task::spawn_blocking(move || {
            let handle = LogFileReader::open_with_cancel_flag(
                OpenLogRequest {
                    location: source.location.clone(),
                    label: source.file_name.clone(),
                    default_encoding: scope.default_encoding.clone(),
                    archive_passwords: scope.archive_passwords.clone(),
                },
                cancel_flag.clone(),
            )?;
            if center >= handle.line_count() {
                anyhow::bail!("事件块中心行超过日志总行数");
            }
            let event_start = find_event_start(&handle, center, &cancel_flag)?;
            let event_end = find_event_end(&handle, event_start, &cancel_flag)?;
            let selection = select_event_range(event_start, event_end, center, max_lines);
            let block = handle.lines(selection.start, selection.end - selection.start)?;
            // 去重签名必须始终取真实事件首行；返回片段从中间裁剪时不能误用普通栈帧。
            let signature = handle
                .lines(event_start, 1)?
                .first()
                .map(|line| normalize_event_signature(&line.text))
                .unwrap_or_default();
            let mut occurrence_count = 0_usize;
            let mut occurrence_lines = Vec::new();
            let mut start = 0_usize;
            while start < handle.line_count() {
                if cancel_flag.load(Ordering::Relaxed) {
                    anyhow::bail!("事件块分析已取消");
                }
                let lines = handle.lines(start, ADVANCED_SCAN_BATCH_LINES)?;
                if lines.is_empty() {
                    break;
                }
                for line in &lines {
                    if !is_event_continuation(&line.text)
                        && normalize_event_signature(&line.text) == signature
                    {
                        occurrence_count = occurrence_count.saturating_add(1);
                        if occurrence_lines.len() < 20 {
                            occurrence_lines.push(line.line_number + 1);
                        }
                    }
                }
                start += lines.len();
            }
            anyhow::Ok((
                handle.byte_len(),
                block,
                signature,
                occurrence_count,
                occurrence_lines,
                selection.head_truncated,
                selection.tail_truncated,
            ))
        })
        .await
        .map_err(|error| AgentToolError::new(format!("事件块提取任务异常结束：{error}")))?
        .map_err(|error| {
            AgentToolError::new(format!(
                "无法提取事件块：{}",
                redact_error_path(error.to_string())
            ))
        })?;
        reconcile_tool_scan(&self.0, scan_bytes, extracted.0)?;
        let expected_lines = extracted.1.len();
        let mut raw_bytes = 0_usize;
        let mut byte_truncated = false;
        let mut lines = Vec::new();
        for line in extracted.1 {
            let remaining = max_bytes.saturating_sub(raw_bytes);
            if remaining == 0 {
                byte_truncated = true;
                break;
            }
            let (text, was_truncated) =
                truncate_text_to_total_bytes(redact_sensitive_text(&line.text), remaining);
            if text.is_empty() && !line.text.is_empty() {
                byte_truncated = true;
                break;
            }
            raw_bytes = raw_bytes.saturating_add(text.len());
            lines.push(EventBlockLineOutput {
                line: line.line_number + 1,
                text,
            });
            if was_truncated {
                byte_truncated = true;
                break;
            }
        }
        self.0
            .budget
            .consume_raw_log_bytes(raw_bytes)
            .map_err(AgentToolError::new)?;
        self.0.publish_budget();
        if let (Some(first), Some(last)) = (lines.first(), lines.last()) {
            self.0
                .evidence_ranges
                .record(&args.source_ref, first.line, last.line)
                .map_err(AgentToolError::new)?;
        }
        for line in &extracted.4 {
            self.0
                .evidence_ranges
                .record(&args.source_ref, *line, *line)
                .map_err(AgentToolError::new)?;
        }
        let start_line = lines.first().map(|line| line.line).unwrap_or(args.line);
        let end_line = lines.last().map(|line| line.line).unwrap_or(args.line);
        checked_output(ExtractEventBlocksOutput {
            source_ref: args.source_ref,
            start_line,
            end_line,
            fingerprint: short_sha256(&extracted.2),
            occurrence_count: extracted.3,
            occurrence_lines: extracted.4,
            truncated: extracted.5 || extracted.6 || byte_truncated || lines.len() < expected_lines,
            head_truncated: extracted.5,
            tail_truncated: extracted.6,
            lines,
            deduplication_basis: "normalized_start_line",
        })
    }
}

/// 事件真实边界裁剪后的半开行区间。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EventBlockRange {
    /// 返回片段起始 0 基行号。
    start: usize,
    /// 返回片段结束 0 基排他行号。
    end: usize,
    /// 是否省略了事件首部。
    head_truncated: bool,
    /// 是否省略了事件尾部。
    tail_truncated: bool,
}

/// 从锚点向前分块定位真实事件首行，长堆栈也不会受固定窗口限制。
fn find_event_start(
    handle: &LogReaderHandle,
    center: usize,
    cancel_flag: &AtomicBool,
) -> anyhow::Result<usize> {
    let mut cursor_end = center.saturating_add(1);
    loop {
        if cancel_flag.load(Ordering::Relaxed) {
            anyhow::bail!("事件块分析已取消");
        }
        let chunk_start = cursor_end.saturating_sub(ADVANCED_SCAN_BATCH_LINES);
        let lines = handle.lines(chunk_start, cursor_end - chunk_start)?;
        if let Some(line) = lines
            .iter()
            .rev()
            .find(|line| !is_event_continuation(&line.text))
        {
            return Ok(line.line_number);
        }
        if chunk_start == 0 {
            return Ok(0);
        }
        cursor_end = chunk_start;
    }
}

/// 从真实事件首行向后分块定位首个非连续行，返回事件排他结束行号。
fn find_event_end(
    handle: &LogReaderHandle,
    event_start: usize,
    cancel_flag: &AtomicBool,
) -> anyhow::Result<usize> {
    let mut cursor = event_start.saturating_add(1);
    while cursor < handle.line_count() {
        if cancel_flag.load(Ordering::Relaxed) {
            anyhow::bail!("事件块分析已取消");
        }
        let lines = handle.lines(
            cursor,
            (handle.line_count() - cursor).min(ADVANCED_SCAN_BATCH_LINES),
        )?;
        if lines.is_empty() {
            break;
        }
        if let Some(line) = lines.iter().find(|line| !is_event_continuation(&line.text)) {
            return Ok(line.line_number);
        }
        cursor = cursor.saturating_add(lines.len());
    }
    Ok(handle.line_count())
}

/// 把真实事件边界裁剪到最大行数，并保证结果始终包含用户提供的锚点行。
fn select_event_range(
    event_start: usize,
    event_end: usize,
    center: usize,
    max_lines: usize,
) -> EventBlockRange {
    let max_lines = max_lines.max(1);
    let event_end = event_end.max(event_start.saturating_add(1));
    if event_end - event_start <= max_lines {
        return EventBlockRange {
            start: event_start,
            end: event_end,
            head_truncated: false,
            tail_truncated: false,
        };
    }
    let latest_start = event_end - max_lines;
    let start = center
        .saturating_sub(max_lines / 2)
        .clamp(event_start, latest_start);
    let end = start.saturating_add(max_lines).min(event_end);
    EventBlockRange {
        start,
        end,
        head_truncated: start > event_start,
        tail_truncated: end < event_end,
    }
}

/// 判断一行是否属于前一条异常或多行事件的延续。
fn is_event_continuation(line: &str) -> bool {
    let trimmed = line.trim_start();
    line.trim().is_empty()
        || line.starts_with([' ', '\t'])
        || trimmed.starts_with("at ")
        || trimmed.starts_with("Caused by:")
        || trimmed.starts_with("Suppressed:")
        || (trimmed.starts_with("...") && trimmed.ends_with("more"))
        || trimmed.starts_with("^ ")
}

/// 通用事件聚合参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct AggregateLogEventsArgs {
    /// 来源引用；为空时扫描全部授权来源。
    #[serde(default)]
    pub source_refs: Vec<String>,
    /// 需要聚合的级别；为空时使用 WARN、ERROR、FATAL、CRITICAL。
    #[serde(default)]
    pub levels: Vec<String>,
    /// 时间桶分钟数，范围 1～1440。
    #[serde(default = "default_time_bucket_minutes")]
    pub time_bucket_minutes: u32,
    /// 最多返回的错误签名数量，范围 1～50。
    #[serde(default = "default_signature_limit")]
    pub max_signatures: usize,
    /// 最多返回的时间桶数量，范围 1～200。
    #[serde(default = "default_time_bucket_limit")]
    pub max_time_buckets: usize,
}

/// 聚合事件的证据位置。
#[derive(Clone, Debug, Serialize)]
struct AggregateEvidenceOutput {
    /// 不透明来源引用。
    source_ref: String,
    /// 1 基行号。
    line: usize,
}

/// 一个时间桶的事件数量。
#[derive(Debug, Serialize)]
struct TimeBucketOutput {
    /// 不执行用户时区推断的标准化分钟标签。
    bucket: String,
    /// 当前桶内匹配事件数量。
    count: usize,
}

/// 一个归一化事件签名的统计。
#[derive(Debug, Serialize)]
struct EventSignatureOutput {
    /// 签名短 SHA-256 ID。
    signature_id: String,
    /// 规范化日志级别。
    level: String,
    /// 出现次数；高基数候选发生替换时是带误差上界的 Space-Saving 估计值。
    count: usize,
    /// 估计值可能高于真实计数的最大数量；为零时计数精确。
    count_error_upper_bound: usize,
    /// 当前候选进入有界表后保留的最早证据；近似计数时不保证是全局首次。
    first: AggregateEvidenceOutput,
    /// 最后出现证据。
    last: AggregateEvidenceOutput,
    /// 获得原文授权时返回的脱敏代表文本。
    representative: Option<String>,
}

/// 通用事件聚合输出。
#[derive(Debug, Serialize)]
pub(crate) struct AggregateLogEventsOutput {
    /// 实际扫描文件数。
    scanned_files: usize,
    /// 实际扫描行数。
    scanned_lines: usize,
    /// 按级别统计的事件数量。
    level_counts: BTreeMap<String, usize>,
    /// 按数量筛选后的时间桶。
    time_buckets: Vec<TimeBucketOutput>,
    /// Top-N 归一化事件签名。
    signatures: Vec<EventSignatureOutput>,
    /// 识别到级别但没有通用时间戳的行数。
    unparsed_timestamp_lines: usize,
    /// 有界 Space-Saving 表发生候选替换的次数，不代表不同签名数量。
    replaced_signature_candidates: usize,
    /// 当前返回的 Top-N 签名中是否存在近似计数。
    signature_counts_approximate: bool,
    /// 已脱敏读取错误。
    errors: Vec<String>,
    /// 时间解释边界。
    timestamp_boundary: &'static str,
}

/// 本地解析常见时间戳和日志级别，返回趋势、Top 签名及可复核证据位置。
#[derive(Clone)]
pub(crate) struct AggregateLogEventsTool(pub Arc<AgentOperationContext>);

impl Tool for AggregateLogEventsTool {
    const NAME: &'static str = "aggregate_log_events";
    type Error = AgentToolError;
    type Args = AggregateLogEventsArgs;
    type Output = AggregateLogEventsOutput;

    fn description(&self) -> String {
        "本地聚合日志级别、时间桶和归一化事件签名；未授权原文时只返回签名 ID 与证据位置。"
            .to_string()
    }

    fn parameters(&self) -> Value {
        schema_value::<Self::Args>()
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let levels = normalize_requested_levels(&args.levels)?;
        let selected = selected_sources(&self.0, &args.source_refs)?;
        let scan_bytes = estimated_full_scan_bytes(&selected);
        self.0
            .begin_tool(Self::NAME, scan_bytes)
            .map_err(AgentToolError::new)?;
        let sources = selected.into_iter().cloned().collect::<Vec<_>>();
        let default_encoding = self.0.scope.default_encoding.clone();
        let archive_passwords = self.0.scope.archive_passwords.clone();
        let allow_raw = self.0.scope.allow_raw_log_content;
        let bucket_minutes = args.time_bucket_minutes.clamp(1, 1440);
        let (cancel_flag, _cancel_guard) = BlockingCancellationGuard::new(&self.0);
        let scan = tokio::task::spawn_blocking(move || {
            scan_aggregate_events(
                sources,
                default_encoding,
                archive_passwords,
                levels,
                bucket_minutes,
                allow_raw,
                cancel_flag,
            )
        })
        .await
        .map_err(|error| AgentToolError::new(format!("事件聚合任务异常结束：{error}")))?;
        reconcile_tool_scan(&self.0, scan_bytes, scan.scanned_bytes)?;

        let replaced_signature_candidates = scan.signatures.replacement_count;
        let mut signatures = scan.signatures.aggregates.into_values().collect::<Vec<_>>();
        signatures.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| left.signature.cmp(&right.signature))
        });
        signatures.truncate(args.max_signatures.clamp(1, 50));
        let mut raw_bytes = 0_usize;
        let signature_outputs = signatures
            .into_iter()
            .map(|signature| {
                raw_bytes = raw_bytes
                    .saturating_add(signature.representative.as_ref().map_or(0, String::len));
                EventSignatureOutput {
                    signature_id: short_sha256(&signature.signature),
                    level: signature.level,
                    count: signature.count,
                    count_error_upper_bound: signature.count_error_upper_bound,
                    first: signature.first,
                    last: signature.last,
                    representative: signature.representative,
                }
            })
            .collect::<Vec<_>>();
        let signature_counts_approximate = signature_outputs
            .iter()
            .any(|signature| signature.count_error_upper_bound > 0);
        if raw_bytes > 0 {
            self.0
                .budget
                .consume_raw_log_bytes(raw_bytes)
                .map_err(AgentToolError::new)?;
            self.0.publish_budget();
        }
        for signature in &signature_outputs {
            self.0
                .evidence_ranges
                .record(
                    &signature.first.source_ref,
                    signature.first.line,
                    signature.first.line,
                )
                .map_err(AgentToolError::new)?;
            self.0
                .evidence_ranges
                .record(
                    &signature.last.source_ref,
                    signature.last.line,
                    signature.last.line,
                )
                .map_err(AgentToolError::new)?;
        }
        let mut time_buckets = scan
            .time_buckets
            .into_iter()
            .map(|(bucket, count)| TimeBucketOutput { bucket, count })
            .collect::<Vec<_>>();
        time_buckets.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| left.bucket.cmp(&right.bucket))
        });
        time_buckets.truncate(args.max_time_buckets.clamp(1, 200));
        time_buckets.sort_by(|left, right| left.bucket.cmp(&right.bucket));

        checked_output(AggregateLogEventsOutput {
            scanned_files: scan.scanned_files,
            scanned_lines: scan.scanned_lines,
            level_counts: scan.level_counts,
            time_buckets,
            signatures: signature_outputs,
            unparsed_timestamp_lines: scan.unparsed_timestamp_lines,
            replaced_signature_candidates,
            signature_counts_approximate,
            errors: scan.errors.into_iter().map(redact_error_path).collect(),
            timestamp_boundary: "带时区时间戳归一化为 UTC；无时区时间戳仅按日志墙钟分桶，不推断时区",
        })
    }
}

/// 一个聚合签名的内部累计状态。
struct EventSignatureAggregate {
    /// 规范化签名原文，只在本地用于分组和哈希。
    signature: String,
    /// 规范化日志级别。
    level: String,
    /// 出现次数。
    count: usize,
    /// Space-Saving 估计计数的误差上界。
    count_error_upper_bound: usize,
    /// 当前候选进入有界表后保留的最早证据。
    first: AggregateEvidenceOutput,
    /// 最后证据。
    last: AggregateEvidenceOutput,
    /// 授权后保留的脱敏短代表文本。
    representative: Option<String>,
}

/// 使用 Space-Saving 算法维护有界高频事件候选，避免高基数日志遗漏后出现的热点。
struct BoundedEventSignatureTable {
    /// 事件键到累计状态的索引。
    aggregates: HashMap<(String, String), EventSignatureAggregate>,
    /// 按估计计数排序的候选键，用于对数复杂度替换当前最小候选。
    frequency_order: BTreeSet<(usize, String, String)>,
    /// 候选表已满后发生替换的次数。
    replacement_count: usize,
    /// 最大候选数量。
    capacity: usize,
}

impl BoundedEventSignatureTable {
    /// 创建至少保留一个候选的有界表。
    fn new(capacity: usize) -> Self {
        Self {
            aggregates: HashMap::new(),
            frequency_order: BTreeSet::new(),
            replacement_count: 0,
            capacity: capacity.max(1),
        }
    }

    /// 记录一次事件；表满时用新候选替换当前最小候选并保留可解释误差上界。
    fn record(
        &mut self,
        level: String,
        signature: String,
        evidence: AggregateEvidenceOutput,
        representative: Option<String>,
    ) {
        let key = (level.clone(), signature.clone());
        if let Some(aggregate) = self.aggregates.get_mut(&key) {
            let previous_count = aggregate.count;
            aggregate.count = aggregate.count.saturating_add(1);
            aggregate.last = evidence;
            let updated_count = aggregate.count;
            self.frequency_order
                .remove(&(previous_count, key.0.clone(), key.1.clone()));
            self.frequency_order.insert((updated_count, key.0, key.1));
            return;
        }

        let (count, count_error_upper_bound) = if self.aggregates.len() < self.capacity {
            (1, 0)
        } else {
            let Some((minimum_count, minimum_level, minimum_signature)) =
                self.frequency_order.iter().next().cloned()
            else {
                return;
            };
            self.frequency_order.remove(&(
                minimum_count,
                minimum_level.clone(),
                minimum_signature.clone(),
            ));
            self.aggregates.remove(&(minimum_level, minimum_signature));
            self.replacement_count = self.replacement_count.saturating_add(1);
            (minimum_count.saturating_add(1), minimum_count)
        };
        self.frequency_order
            .insert((count, level.clone(), signature.clone()));
        self.aggregates.insert(
            key,
            EventSignatureAggregate {
                signature,
                level,
                count,
                count_error_upper_bound,
                first: evidence.clone(),
                last: evidence,
                representative,
            },
        );
    }
}

/// 阻塞事件聚合扫描的完整结果。
struct AggregateEventScanResult {
    /// 实际打开日志字节数。
    scanned_bytes: u64,
    /// 尝试扫描文件数。
    scanned_files: usize,
    /// 实际扫描行数。
    scanned_lines: usize,
    /// 日志级别计数。
    level_counts: BTreeMap<String, usize>,
    /// 时间桶计数。
    time_buckets: BTreeMap<String, usize>,
    /// 归一化签名分组。
    signatures: BoundedEventSignatureTable,
    /// 时间戳无法识别的匹配行数。
    unparsed_timestamp_lines: usize,
    /// 非致命来源错误。
    errors: Vec<String>,
}

/// 顺序扫描授权来源并聚合通用事件；来源和行批次边界均检查取消。
fn scan_aggregate_events(
    sources: Vec<SnapshotSource>,
    default_encoding: String,
    archive_passwords: crate::loader::archive::ArchivePasswordStore,
    levels: BTreeSet<String>,
    bucket_minutes: u32,
    allow_raw: bool,
    cancel_flag: Arc<AtomicBool>,
) -> AggregateEventScanResult {
    let mut result = AggregateEventScanResult {
        scanned_bytes: 0,
        scanned_files: 0,
        scanned_lines: 0,
        level_counts: BTreeMap::new(),
        time_buckets: BTreeMap::new(),
        signatures: BoundedEventSignatureTable::new(MAX_EVENT_SIGNATURE_GROUPS),
        unparsed_timestamp_lines: 0,
        errors: Vec::new(),
    };
    for source in sources {
        if cancel_flag.load(Ordering::Relaxed) {
            break;
        }
        result.scanned_files += 1;
        let handle = match LogFileReader::open_with_cancel_flag(
            OpenLogRequest {
                location: source.location.clone(),
                label: source.file_name.clone(),
                default_encoding: default_encoding.clone(),
                archive_passwords: archive_passwords.clone(),
            },
            cancel_flag.clone(),
        ) {
            Ok(handle) => handle,
            Err(error) => {
                result.errors.push(error.to_string());
                continue;
            }
        };
        result.scanned_bytes = result.scanned_bytes.saturating_add(handle.byte_len());
        let mut start = 0_usize;
        while start < handle.line_count() {
            if cancel_flag.load(Ordering::Relaxed) {
                break;
            }
            let lines = match handle.lines(start, ADVANCED_SCAN_BATCH_LINES) {
                Ok(lines) => lines,
                Err(error) => {
                    result.errors.push(error.to_string());
                    break;
                }
            };
            if lines.is_empty() {
                break;
            }
            for line in &lines {
                let Some(level) = detect_log_level(&line.text) else {
                    continue;
                };
                if !levels.contains(&level) {
                    continue;
                }
                *result.level_counts.entry(level.clone()).or_default() += 1;
                if let Some(bucket) = log_time_bucket(&line.text, bucket_minutes) {
                    *result.time_buckets.entry(bucket).or_default() += 1;
                } else {
                    result.unparsed_timestamp_lines += 1;
                }
                let signature = normalize_event_signature(&line.text);
                let evidence = AggregateEvidenceOutput {
                    source_ref: source.source_ref.clone(),
                    line: line.line_number + 1,
                };
                let representative = allow_raw
                    .then(|| truncate_utf8_with_ellipsis(redact_sensitive_text(&line.text), 240));
                result
                    .signatures
                    .record(level, signature, evidence, representative);
            }
            result.scanned_lines = result.scanned_lines.saturating_add(lines.len());
            start += lines.len();
        }
    }
    result
}

/// 校验并规范化用户请求的日志级别集合。
fn normalize_requested_levels(levels: &[String]) -> Result<BTreeSet<String>, AgentToolError> {
    let values = if levels.is_empty() {
        vec!["WARN", "ERROR", "FATAL", "CRITICAL"]
    } else {
        levels.iter().map(String::as_str).collect::<Vec<_>>()
    };
    if values.len() > 8 {
        return Err(AgentToolError::new("levels 最多包含 8 个日志级别"));
    }
    values
        .into_iter()
        .map(|level| {
            canonical_log_level(level)
                .ok_or_else(|| AgentToolError::new(format!("不支持的日志级别：{level}")))
        })
        .collect()
}

/// 从常见日志格式中识别 TRACE～CRITICAL 级别。
fn detect_log_level(line: &str) -> Option<String> {
    static LEVEL_REGEX: OnceLock<Regex> = OnceLock::new();
    let regex = LEVEL_REGEX.get_or_init(|| {
        Regex::new(r"(?i)(?:^|[\s\[\]])(TRACE|DEBUG|INFO|WARN(?:ING)?|ERROR|FATAL|CRITICAL)(?:$|[\s\]\[:.\-])")
            .expect("内置日志级别正则必须有效")
    });
    let level = regex.captures(line)?.get(1)?.as_str();
    canonical_log_level(level)
}

/// 把日志级别别名规范为稳定大写值。
fn canonical_log_level(level: &str) -> Option<String> {
    match level.trim().to_ascii_uppercase().as_str() {
        "TRACE" => Some("TRACE".to_string()),
        "DEBUG" => Some("DEBUG".to_string()),
        "INFO" => Some("INFO".to_string()),
        "WARN" | "WARNING" => Some("WARN".to_string()),
        "ERROR" => Some("ERROR".to_string()),
        "FATAL" => Some("FATAL".to_string()),
        "CRITICAL" => Some("CRITICAL".to_string()),
        _ => None,
    }
}

/// 解析常见日期时间前缀并返回指定分钟宽度的稳定时间桶。
fn log_time_bucket(line: &str, bucket_minutes: u32) -> Option<String> {
    let timestamp = parse_log_timestamp(line)?;
    let seconds = timestamp.and_utc().timestamp();
    let bucket_seconds = i64::from(bucket_minutes).saturating_mul(60).max(60);
    let bucket = seconds
        .div_euclid(bucket_seconds)
        .saturating_mul(bucket_seconds);
    DateTime::<Utc>::from_timestamp(bucket, 0)
        .map(|value| value.format("%Y-%m-%d %H:%M").to_string())
}

/// 解析 RFC3339 及常见 `yyyy-MM-dd HH:mm:ss`、斜杠日期日志时间戳。
fn parse_log_timestamp(line: &str) -> Option<NaiveDateTime> {
    static TIMESTAMP_REGEX: OnceLock<Regex> = OnceLock::new();
    let regex = TIMESTAMP_REGEX.get_or_init(|| {
        Regex::new(r"^\s*\[?(\d{4}[-/]\d{2}[-/]\d{2}[T ]\d{2}:\d{2}:\d{2}(?:[.,]\d{1,9})?(?:Z|[+-]\d{2}:?\d{2})?)")
            .expect("内置时间戳正则必须有效")
    });
    let raw = regex.captures(line)?.get(1)?.as_str().replace(',', ".");
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(&raw) {
        return Some(timestamp.naive_utc());
    }
    for format in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y/%m/%d %H:%M:%S%.f",
    ] {
        if let Ok(timestamp) = NaiveDateTime::parse_from_str(&raw, format) {
            return Some(timestamp);
        }
    }
    None
}

/// 删除常见易变 ID、数字和空白差异，生成用于本地去重的事件签名。
fn normalize_event_signature(line: &str) -> String {
    static NORMALIZERS: OnceLock<Vec<Regex>> = OnceLock::new();
    let regexes = NORMALIZERS.get_or_init(|| {
        [
            r"^\s*\[?\d{4}[-/]\d{2}[-/]\d{2}[T ]\d{2}:\d{2}:\d{2}(?:[.,]\d{1,9})?(?:Z|[+-]\d{2}:?\d{2})?\]?",
            r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b",
            r"(?i)\b0x[0-9a-f]+\b",
            r"\b\d+\b",
        ]
        .into_iter()
        .map(|pattern| Regex::new(pattern).expect("内置事件签名正则必须有效"))
        .collect()
    });
    let mut value = line.to_string();
    for regex in regexes {
        value = regex.replace_all(&value, "?").into_owned();
    }
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_utf8_with_ellipsis(normalized, 512)
}

/// 声明式会话制品查询参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct QueryArtifactArgs {
    /// `run_analyzer` 返回的会话制品 ID。
    pub artifact_id: String,
    /// 制品根对象中的数组字段，例如 `top_threads` 或 `top_requests`。
    pub collection: String,
    /// 字段精确文本过滤。
    #[serde(default)]
    pub equals: BTreeMap<String, String>,
    /// 字段包含文本过滤。
    #[serde(default)]
    pub contains: BTreeMap<String, String>,
    /// 数值字段最小值过滤。
    #[serde(default)]
    pub minimum: BTreeMap<String, f64>,
    /// 数值字段最大值过滤。
    #[serde(default)]
    pub maximum: BTreeMap<String, f64>,
    /// 可选排序字段。
    #[serde(default)]
    pub sort_by: Option<String>,
    /// 是否按降序排序。
    #[serde(default)]
    pub descending: bool,
    /// 只返回这些顶层字段；为空时返回完整数组项。
    #[serde(default)]
    pub select_fields: Vec<String>,
    /// 过滤后分页偏移。
    #[serde(default)]
    pub offset: usize,
    /// 返回项目数，范围 1～100。
    #[serde(default = "default_artifact_query_limit")]
    pub limit: usize,
}

/// 声明式制品查询输出。
#[derive(Debug, Serialize)]
pub(crate) struct QueryArtifactOutput {
    /// 过滤后的项目总数。
    total: usize,
    /// 当前页 JSON 数组项。
    items: Vec<Value>,
    /// 下一页偏移。
    next_offset: Option<usize>,
}

/// 对当前会话制品执行固定字段过滤、排序、投影和分页，不接受脚本或动态表达式。
#[derive(Clone)]
pub(crate) struct QueryArtifactTool(pub Arc<AgentOperationContext>);

impl Tool for QueryArtifactTool {
    const NAME: &'static str = "query_artifact";
    type Error = AgentToolError;
    type Args = QueryArtifactArgs;
    type Output = QueryArtifactOutput;

    fn description(&self) -> String {
        "以声明式字段条件查询分析制品数组，支持过滤、排序、投影和分页；不执行 jq 或脚本。"
            .to_string()
    }

    fn parameters(&self) -> Value {
        schema_value::<Self::Args>()
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        self.0
            .begin_tool(Self::NAME, 0)
            .map_err(AgentToolError::new)?;
        validate_artifact_query(&args)?;
        let store = self
            .0
            .artifacts
            .lock()
            .map_err(|_| AgentToolError::new("分析制品存储已损坏"))?;
        let artifact = store
            .get(&args.artifact_id)
            .ok_or_else(|| AgentToolError::new("artifact_id 不存在或不属于当前会话"))?;
        let value: Value = serde_json::from_str(artifact)
            .map_err(|_| AgentToolError::new("分析制品不是有效 JSON"))?;
        let collection = value
            .get(&args.collection)
            .and_then(Value::as_array)
            .ok_or_else(|| AgentToolError::new("collection 不存在或不是数组"))?;
        let mut items = collection
            .iter()
            .filter(|item| artifact_item_matches(item, &args))
            .cloned()
            .collect::<Vec<_>>();
        if let Some(sort_by) = args.sort_by.as_deref() {
            items.sort_by(|left, right| compare_json_fields(left, right, sort_by));
            if args.descending {
                items.reverse();
            }
        }
        let total = items.len();
        let limit = args.limit.clamp(1, 100);
        let end = args.offset.saturating_add(limit).min(total);
        let page = items
            .get(args.offset..end)
            .unwrap_or_default()
            .iter()
            .map(|item| project_artifact_item(item, &args.select_fields))
            .collect::<Vec<_>>();
        checked_output(QueryArtifactOutput {
            total,
            items: page,
            next_offset: (end < total).then_some(end),
        })
    }
}

/// 校验制品查询只包含有限的顶层字段名和过滤条件。
fn validate_artifact_query(args: &QueryArtifactArgs) -> Result<(), AgentToolError> {
    if !is_safe_artifact_field(&args.collection) {
        return Err(AgentToolError::new("collection 必须是安全的顶层字段名"));
    }
    let filter_count =
        args.equals.len() + args.contains.len() + args.minimum.len() + args.maximum.len();
    if filter_count > 12 {
        return Err(AgentToolError::new("制品查询最多包含 12 个过滤条件"));
    }
    if args.select_fields.len() > 20 {
        return Err(AgentToolError::new("制品查询最多投影 20 个字段"));
    }
    for field in args
        .equals
        .keys()
        .chain(args.contains.keys())
        .chain(args.minimum.keys())
        .chain(args.maximum.keys())
        .chain(args.sort_by.iter())
        .chain(args.select_fields.iter())
    {
        if !is_safe_artifact_field(field) {
            return Err(AgentToolError::new(format!("制品字段名无效：{field}")));
        }
    }
    if args
        .equals
        .values()
        .chain(args.contains.values())
        .any(|value| value.len() > 256)
    {
        return Err(AgentToolError::new("制品文本过滤值不能超过 256 B"));
    }
    Ok(())
}

/// 判断字符串是否为不包含路径或表达式语法的安全顶层字段名。
fn is_safe_artifact_field(field: &str) -> bool {
    !field.is_empty()
        && field.len() <= 64
        && field
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// 判断一个 JSON 数组项是否满足全部声明式过滤条件。
fn artifact_item_matches(item: &Value, args: &QueryArtifactArgs) -> bool {
    let Some(object) = item.as_object() else {
        return false;
    };
    args.equals.iter().all(|(field, expected)| {
        object
            .get(field)
            .and_then(json_scalar_text)
            .is_some_and(|value| value == *expected)
    }) && args.contains.iter().all(|(field, expected)| {
        object
            .get(field)
            .and_then(json_scalar_text)
            .is_some_and(|value| value.to_lowercase().contains(&expected.to_lowercase()))
    }) && args.minimum.iter().all(|(field, minimum)| {
        object
            .get(field)
            .and_then(Value::as_f64)
            .is_some_and(|value| value >= *minimum)
    }) && args.maximum.iter().all(|(field, maximum)| {
        object
            .get(field)
            .and_then(Value::as_f64)
            .is_some_and(|value| value <= *maximum)
    })
}

/// 把 JSON 标量转换为稳定文本；对象和数组不参与文本过滤。
fn json_scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Null => Some("null".to_string()),
        Value::Array(_) | Value::Object(_) => None,
    }
}

/// 比较两个 JSON 项的指定顶层字段；数值优先，随后使用标量文本。
fn compare_json_fields(left: &Value, right: &Value, field: &str) -> CmpOrdering {
    let left = left.get(field);
    let right = right.get(field);
    match (left.and_then(Value::as_f64), right.and_then(Value::as_f64)) {
        (Some(left), Some(right)) => left.total_cmp(&right),
        _ => left
            .and_then(json_scalar_text)
            .cmp(&right.and_then(json_scalar_text)),
    }
}

/// 按字段白名单投影一个制品数组项；非对象项原样返回。
fn project_artifact_item(item: &Value, fields: &[String]) -> Value {
    if fields.is_empty() {
        return item.clone();
    }
    let Some(object) = item.as_object() else {
        return item.clone();
    };
    let projected = fields
        .iter()
        .filter_map(|field| {
            object
                .get(field)
                .map(|value| (field.clone(), value.clone()))
        })
        .collect::<Map<_, _>>();
    Value::Object(projected)
}

/// 返回任意文本的前 16 个十六进制 SHA-256 字符。
fn short_sha256(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))[..16].to_string()
}

/// 把 Schemars 参数 Schema 转成 Rig 所需 JSON 值。
fn schema_value<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T))
        .unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
}

/// 来源概览默认日志类型页大小。
fn default_overview_profile_limit() -> usize {
    10
}

/// 来源概览默认扩展名分组数量。
fn default_extension_limit() -> usize {
    20
}

/// 批量搜索每个模式默认代表性结果数。
fn default_batch_results_per_pattern() -> usize {
    5
}

/// 日志采样默认行数。
fn default_sample_lines() -> usize {
    30
}

/// 日志采样默认原文字节数。
fn default_sample_bytes() -> usize {
    32 * 1024
}

/// 事件块默认最大行数。
fn default_event_block_lines() -> usize {
    120
}

/// 事件块默认原文字节数。
fn default_event_block_bytes() -> usize {
    MAX_TOOL_RAW_BYTES
}

/// 通用事件聚合默认时间桶宽度。
fn default_time_bucket_minutes() -> u32 {
    5
}

/// 通用事件聚合默认签名数量。
fn default_signature_limit() -> usize {
    20
}

/// 通用事件聚合默认时间桶数量。
fn default_time_bucket_limit() -> usize {
    100
}

/// 制品声明式查询默认页大小。
fn default_artifact_query_limit() -> usize {
    50
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;

    use tempfile::TempDir;

    use super::*;
    use crate::agent::session::{
        AgentBudget, AgentEvidenceStore, AgentOperationContext, LogProfileSnapshot,
        SourceScopeSnapshot,
    };
    use crate::config::{LoaderConfig, LogNameMatcher, LogNameMatcherMode, LogNameMatcherTarget};
    use crate::loader::archive::ArchivePasswordStore;
    use crate::loader::{SourceId, SourceLocation};

    /// 构造只授权一个临时日志来源的工具运行上下文。
    fn test_context(
        content: &str,
        allow_raw_log_content: bool,
    ) -> (Arc<AgentOperationContext>, TempDir, String) {
        let directory = tempfile::tempdir().expect("应创建临时日志目录");
        let path = directory.path().join("application.log");
        fs::write(&path, content).expect("应写入临时日志");
        let source_ref = "opaque-source".to_string();
        let scope = SourceScopeSnapshot {
            session_id: "test-session".to_string(),
            root_label: "test-root".to_string(),
            sources: Arc::new(vec![SnapshotSource {
                source_ref: source_ref.clone(),
                source_id: SourceId(1),
                file_name: "application.log".to_string(),
                relative_path: "application.log".to_string(),
                location: SourceLocation::LocalPath(path),
                size: Some(content.len() as u64),
                profile_id: None,
            }]),
            profiles: Arc::new(HashMap::new()),
            default_encoding: "UTF-8".to_string(),
            loader_config: LoaderConfig::default(),
            archive_passwords: ArchivePasswordStore::default(),
            allow_raw_log_content,
        };
        let (event_sender, _event_receiver) = async_channel::bounded(64);
        let context = Arc::new(AgentOperationContext {
            scope: Arc::new(scope),
            budget: Arc::new(AgentBudget::balanced()),
            cancellation: tokio_util::sync::CancellationToken::new(),
            event_sender,
            report: Mutex::new(None),
            artifacts: Mutex::new(HashMap::new()),
            evidence_ranges: AgentEvidenceStore::default(),
            used_log_profiles: Mutex::new(BTreeSet::new()),
            question: "测试问题".to_string(),
            pending_user_messages: Arc::new(AtomicUsize::new(0)),
        });
        (context, directory, source_ref)
    }

    /// 在隔离的单线程 Tokio 运行时执行异步工具测试。
    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("应创建测试运行时")
    }

    /// 验证均匀采样同时覆盖首尾并保持行号递增。
    #[test]
    fn uniform_sampling_covers_first_and_last_lines() {
        assert_eq!(
            sample_line_indices(10, 4, LogSampleStrategy::Uniform),
            vec![0, 3, 6, 9]
        );
    }

    /// 验证来源概览会区分逐规则命中、任意规则命中和优先级决胜后的最终采用数量。
    #[test]
    fn source_overview_reports_matcher_and_selected_counts() {
        let profile = LogProfileSnapshot {
            profile_id: "memory-profile".to_string(),
            name: "内存日志".to_string(),
            priority: 100,
            matchers: vec![
                LogNameMatcher {
                    target: LogNameMatcherTarget::FileName,
                    mode: LogNameMatcherMode::Prefix,
                    pattern: "memory_".to_string(),
                    case_sensitive: false,
                },
                LogNameMatcher {
                    target: LogNameMatcherTarget::FileName,
                    mode: LogNameMatcherMode::Suffix,
                    pattern: ".log".to_string(),
                    case_sensitive: false,
                },
            ],
            description: "内存监控日志".to_string(),
            description_sha256: "digest".to_string(),
        };
        let sources = vec![
            SnapshotSource {
                source_ref: "memory-source".to_string(),
                source_id: SourceId(1),
                file_name: "memory_20260715.log".to_string(),
                relative_path: "monitor/memory_20260715.log".to_string(),
                location: SourceLocation::LocalPath(PathBuf::from("memory_20260715.log")),
                size: Some(10),
                profile_id: Some(profile.profile_id.clone()),
            },
            SnapshotSource {
                source_ref: "other-source".to_string(),
                source_id: SourceId(2),
                file_name: "application.log".to_string(),
                relative_path: "application.log".to_string(),
                location: SourceLocation::LocalPath(PathBuf::from("application.log")),
                size: Some(20),
                profile_id: None,
            },
        ];

        let overview = profile_overview(&profile, &sources);

        assert_eq!(overview.rules[0].matched_file_count, 1);
        assert_eq!(overview.rules[1].matched_file_count, 2);
        assert_eq!(overview.matched_file_count, 2);
        assert_eq!(overview.selected_file_count, 1);
        assert_eq!(overview.source_refs, vec!["memory-source"]);
    }

    /// 验证有界采样返回真实行号并登记为可提交报告的证据。
    #[test]
    fn sample_tool_returns_and_registers_real_lines() {
        let (context, _directory, source_ref) = test_context("first\nsecret=abc\nlast\n", true);
        let output = test_runtime()
            .block_on(SampleLogTool(context.clone()).call(SampleLogArgs {
                source_ref: source_ref.clone(),
                strategy: LogSampleStrategy::Tail,
                max_lines: 2,
                max_bytes: 4096,
            }))
            .expect("日志采样应成功");

        assert_eq!(output.lines.len(), 2);
        assert_eq!(output.lines[0].line, 2);
        assert!(output.lines[0].text.contains("[REDACTED]"));
        assert!(context.evidence_ranges.contains(&source_ref, 2, 2).unwrap());
    }

    /// 验证批量搜索一次返回每个模式的独立计数和代表性证据。
    #[test]
    fn batch_search_reports_each_pattern_from_one_scan() {
        let (context, _directory, source_ref) =
            test_context("INFO start\nERROR timeout\nWARN retry\nERROR retry\n", true);
        let output = test_runtime()
            .block_on(SearchLogsBatchTool(context).call(SearchLogsBatchArgs {
                patterns: vec![
                    BatchSearchPatternArgs {
                        pattern_id: "error".to_string(),
                        query: "ERROR".to_string(),
                        regex: false,
                        case_sensitive: false,
                    },
                    BatchSearchPatternArgs {
                        pattern_id: "retry".to_string(),
                        query: "retry".to_string(),
                        regex: false,
                        case_sensitive: false,
                    },
                ],
                source_refs: vec![source_ref],
                max_results_per_pattern: 5,
            }))
            .expect("批量搜索应成功");

        assert_eq!(output.scanned_files, 1);
        assert_eq!(output.patterns[0].matched_lines, 2);
        assert_eq!(output.patterns[1].matched_lines, 2);
    }

    /// 验证批量搜索在进入扫描预算前拒绝无法编译的 Rust 正则。
    #[test]
    fn batch_search_rejects_invalid_regex_before_scanning() {
        let error = validate_batch_patterns(&[BatchSearchPatternArgs {
            pattern_id: "broken".to_string(),
            query: "[".to_string(),
            regex: true,
            case_sensitive: false,
        }])
        .expect_err("无效正则必须被拒绝");

        assert!(error.to_string().contains("broken"));
    }

    /// 验证合法的最大批量搜索在原文预算耗尽时返回计数和部分命中，而不是整体失败。
    #[test]
    fn batch_search_returns_partial_results_at_raw_budget() {
        let keywords = (0..20)
            .map(|index| format!("PATTERN_{index}"))
            .collect::<Vec<_>>();
        let long_line = format!("{} {}\n", keywords.join(" "), "x".repeat(5000));
        let content = long_line.repeat(20);
        let (context, _directory, source_ref) = test_context(&content, true);
        let output = test_runtime()
            .block_on(
                SearchLogsBatchTool(context).call(SearchLogsBatchArgs {
                    patterns: keywords
                        .into_iter()
                        .enumerate()
                        .map(|(index, query)| BatchSearchPatternArgs {
                            pattern_id: format!("pattern_{index}"),
                            query,
                            regex: false,
                            case_sensitive: true,
                        })
                        .collect(),
                    source_refs: vec![source_ref],
                    max_results_per_pattern: 20,
                }),
            )
            .expect("达到原文预算后仍应返回部分批量结果");

        assert!(
            output
                .patterns
                .iter()
                .all(|pattern| pattern.matched_lines == 20)
        );
        assert!(output.patterns.iter().any(|pattern| pattern.truncated));
        let raw_bytes = output
            .patterns
            .iter()
            .flat_map(|pattern| &pattern.hits)
            .filter_map(|hit| hit.text.as_ref())
            .map(String::len)
            .sum::<usize>();
        assert!(raw_bytes <= MAX_TOOL_RAW_BYTES);
    }

    /// 验证长事件被裁剪时仍保留锚点，并同时标记首尾截断。
    #[test]
    fn event_range_keeps_anchor_when_long_event_is_truncated() {
        let range = select_event_range(10, 510, 260, 200);

        assert!(range.start <= 260 && 260 < range.end);
        assert_eq!(range.end - range.start, 200);
        assert!(range.head_truncated);
        assert!(range.tail_truncated);
    }

    /// 验证事件块工具返回完整栈、重复次数并登记首末行证据。
    #[test]
    fn event_block_tool_extracts_repeated_stack_and_evidence() {
        let (context, _directory, source_ref) = test_context(
            "ERROR request 1 failed\n    at example.Service.run(Service.java:1)\nINFO retry\nERROR request 2 failed\n    at example.Service.run(Service.java:1)\n",
            true,
        );
        let output = test_runtime()
            .block_on(
                ExtractEventBlocksTool(context.clone()).call(ExtractEventBlocksArgs {
                    source_ref: source_ref.clone(),
                    line: 2,
                    max_lines: 20,
                    max_bytes: 4096,
                }),
            )
            .expect("事件块提取应成功");

        assert_eq!(output.start_line, 1);
        assert_eq!(output.end_line, 2);
        assert_eq!(output.occurrence_count, 2);
        assert_eq!(output.occurrence_lines, vec![1, 4]);
        assert!(context.evidence_ranges.contains(&source_ref, 1, 2).unwrap());
    }

    /// 验证从超长堆栈深处提取时返回正文包含锚点，并继续使用真实事件首行生成指纹。
    #[test]
    fn event_block_tool_keeps_deep_anchor() {
        let mut content = String::from("ERROR deep failure\n");
        for index in 0..500 {
            content.push_str(&format!("    at example.Frame{index}.run(Frame.java:1)\n"));
        }
        content.push_str("INFO recovered\n");
        let (context, _directory, source_ref) = test_context(&content, true);
        let output = test_runtime()
            .block_on(
                ExtractEventBlocksTool(context).call(ExtractEventBlocksArgs {
                    source_ref,
                    line: 401,
                    max_lines: 20,
                    max_bytes: 64 * 1024,
                }),
            )
            .expect("超长事件块提取应成功");

        assert!(output.start_line <= 401 && 401 <= output.end_line);
        assert!(output.lines.iter().any(|line| line.line == 401));
        assert!(output.truncated);
        assert!(output.head_truncated);
        assert!(output.tail_truncated);
        assert_eq!(output.occurrence_count, 1);
        assert_eq!(output.occurrence_lines, vec![1]);
    }

    /// 验证常见时间戳和级别能够生成稳定五分钟桶。
    #[test]
    fn event_aggregation_parses_level_and_time_bucket() {
        let line = "2026-07-16 12:07:31.123 [worker] ERROR request failed";
        assert_eq!(detect_log_level(line).as_deref(), Some("ERROR"));
        assert_eq!(
            log_time_bucket(line, 5).as_deref(),
            Some("2026-07-16 12:05")
        );
    }

    /// 验证无原文授权时事件聚合仍返回计数和证据，但不返回代表正文。
    #[test]
    fn aggregate_tool_hides_representative_without_raw_consent() {
        let (context, _directory, source_ref) = test_context(
            "2026-07-16 12:01:00 ERROR request 1 failed\n2026-07-16 12:02:00 ERROR request 2 failed\n",
            false,
        );
        let output = test_runtime()
            .block_on(
                AggregateLogEventsTool(context).call(AggregateLogEventsArgs {
                    source_refs: vec![source_ref],
                    levels: vec!["ERROR".to_string()],
                    time_bucket_minutes: 5,
                    max_signatures: 10,
                    max_time_buckets: 10,
                }),
            )
            .expect("事件聚合应成功");

        assert_eq!(output.level_counts.get("ERROR"), Some(&2));
        assert_eq!(output.signatures.len(), 1);
        assert_eq!(output.signatures[0].count, 2);
        assert!(output.signatures[0].representative.is_none());
    }

    /// 验证候选表满后出现的高频签名能够替换低频候选，并暴露近似计数误差。
    #[test]
    fn bounded_signature_table_keeps_late_heavy_hitter() {
        let mut table = BoundedEventSignatureTable::new(2);
        for (index, signature) in ["first", "second", "hot", "hot", "hot"]
            .into_iter()
            .enumerate()
        {
            table.record(
                "ERROR".to_string(),
                signature.to_string(),
                AggregateEvidenceOutput {
                    source_ref: "source".to_string(),
                    line: index + 1,
                },
                None,
            );
        }

        let hot = table
            .aggregates
            .get(&("ERROR".to_string(), "hot".to_string()))
            .expect("后出现的高频签名应保留在候选表中");
        assert_eq!(hot.count, 4);
        assert_eq!(hot.count_error_upper_bound, 1);
        assert_eq!(table.replacement_count, 1);
    }

    /// 验证事件签名会抹平 UUID、十六进制地址和普通数字差异。
    #[test]
    fn event_signature_normalizes_volatile_values() {
        let first = normalize_event_signature(
            "2026-07-16 12:00:00 ERROR id=550e8400-e29b-41d4-a716-446655440000 ptr=0xabc count=42",
        );
        let second = normalize_event_signature(
            "2026-07-16 12:01:00 ERROR id=123e4567-e89b-12d3-a456-426614174000 ptr=0xdef count=99",
        );
        assert_eq!(first, second);
    }

    /// 验证制品查询只匹配满足文本和数值条件的对象。
    #[test]
    fn artifact_filter_is_declarative_and_deterministic() {
        let item = serde_json::json!({"name":"worker-timeout", "count": 8});
        let args = QueryArtifactArgs {
            artifact_id: "artifact".to_string(),
            collection: "items".to_string(),
            equals: BTreeMap::new(),
            contains: BTreeMap::from([("name".to_string(), "TIME".to_string())]),
            minimum: BTreeMap::from([("count".to_string(), 5.0)]),
            maximum: BTreeMap::new(),
            sort_by: None,
            descending: false,
            select_fields: Vec::new(),
            offset: 0,
            limit: 10,
        };
        assert!(artifact_item_matches(&item, &args));
    }

    /// 验证制品工具能够过滤、排序并投影当前会话中的结构化数组。
    #[test]
    fn query_artifact_filters_sorts_and_projects_items() {
        let (context, _directory, _source_ref) = test_context("INFO ready\n", false);
        context.artifacts.lock().unwrap().insert(
            "artifact".to_string(),
            serde_json::json!({
                "items": [
                    {"name": "slow-a", "count": 2, "ignored": true},
                    {"name": "slow-b", "count": 8, "ignored": false},
                    {"name": "fast", "count": 20, "ignored": false}
                ]
            })
            .to_string(),
        );
        let output = test_runtime()
            .block_on(QueryArtifactTool(context).call(QueryArtifactArgs {
                artifact_id: "artifact".to_string(),
                collection: "items".to_string(),
                equals: BTreeMap::new(),
                contains: BTreeMap::from([("name".to_string(), "slow".to_string())]),
                minimum: BTreeMap::from([("count".to_string(), 1.0)]),
                maximum: BTreeMap::new(),
                sort_by: Some("count".to_string()),
                descending: true,
                select_fields: vec!["name".to_string(), "count".to_string()],
                offset: 0,
                limit: 10,
            }))
            .expect("制品查询应成功");

        assert_eq!(output.total, 2);
        assert_eq!(output.items[0]["name"], "slow-b");
        assert!(output.items[0].get("ignored").is_none());
    }
}
