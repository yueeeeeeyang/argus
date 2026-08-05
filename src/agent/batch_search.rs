//! 文件职责：为 AI Agent 提供独立的流式多模式批量日志搜索引擎。
//! 创建日期：2026-07-17
//! 修改日期：2026-07-17
//! 作者：Argus 开发团队
//! 主要功能：流式解码日志、使用 Aho-Corasick/RegexSet 一次匹配多模式，并按物理归档容器单次遍历。

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use aho_corasick::{AhoCorasick, AhoCorasickBuilder};
use anyhow::{Context as _, Result, anyhow, bail};
use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::{CoderResult, Encoding, UTF_8, UTF_16BE, UTF_16LE};
use regex::{RegexSet, RegexSetBuilder};

use crate::agent::log_search::AgentSearchPattern;
use crate::agent::session::{SnapshotSource, SourceScopeSnapshot};
use crate::loader::SourceLocation;
use crate::loader::archive::detector::{ArchiveFormat, detect_archive_format_by_name};
use crate::loader::archive::password::{ArchivePasswordKey, annotate_archive_password_error};
use crate::loader::archive::registry::archive_registry;
use crate::utils::path::normalize_archive_entry_path;

/// 单次底层读取块大小；每块结束时检查取消状态并把完整行交给匹配器。
const BATCH_SEARCH_READ_CHUNK_BYTES: usize = 64 * 1024;
/// 流式编码检测样本上限；仅缓存当前正在读取的一个日志，不建立全文副本或行索引。
const BATCH_SEARCH_ENCODING_SAMPLE_BYTES: usize = 256 * 1024;

/// 批量搜索保留的一条原始证据；只有每个模式的前 N 条命中会分配正文。
#[derive(Clone, Debug)]
pub(crate) struct AgentBatchSearchHit {
    /// 当前会话不透明来源引用。
    pub source_ref: String,
    /// 多根范围内唯一展示路径。
    pub relative_path: String,
    /// 0 基行号。
    pub line_number: usize,
    /// 未脱敏原始行正文。
    pub line_text: String,
}

/// 单个模式的完整计数和有界代表性命中。
#[derive(Clone, Debug, Default)]
pub(crate) struct AgentBatchPatternResult {
    /// 全部已扫描日志中的命中行数；同一行同一模式只计数一次。
    pub matched_lines: usize,
    /// 仅保留调用方要求的前若干条命中。
    pub hits: Vec<AgentBatchSearchHit>,
}

/// 独立流式批量搜索的本地执行统计。
#[derive(Clone, Debug, Default)]
pub(crate) struct AgentBatchSearchSummary {
    /// 已实际打开并尝试扫描的日志数。
    pub scanned_files: usize,
    /// 已交给匹配器的日志行数。
    pub scanned_lines: usize,
    /// 已消费的解压后日志原始字节数，不重复计算外层容器字节。
    pub scanned_bytes: u64,
    /// 至少命中一个模式的日志行数。
    pub matched_results: usize,
    /// 单个来源或容器失败后保留的非致命错误。
    pub errors: Vec<String>,
    /// 是否在读取块或容器边界响应用户取消。
    pub was_cancelled: bool,
    /// 实际创建的物理或嵌套容器读取器数量，供性能回归测试确认单次打开语义。
    pub opened_containers: usize,
}

/// 一次批量搜索的完整返回值；模式结果顺序与输入严格一致。
#[derive(Clone, Debug, Default)]
pub(crate) struct AgentBatchSearchOutput {
    /// 每个输入模式的精确计数与有界命中。
    pub patterns: Vec<AgentBatchPatternResult>,
    /// 本地扫描统计。
    pub summary: AgentBatchSearchSummary,
}

/// 普通关键字自动机分组；映射表把自动机内部 ID 还原成输入模式下标。
struct AhoPatternGroup {
    /// 一次匹配当前分组全部普通关键字的自动机。
    matcher: AhoCorasick,
    /// 自动机模式下标到原始输入下标的映射。
    pattern_indices: Vec<usize>,
}

/// 正则集合分组；一个 RegexSet 一次完成当前大小写语义下的全部正则匹配。
struct RegexPatternGroup {
    /// 编译后的正则集合。
    matcher: RegexSet,
    /// RegexSet 模式下标到原始输入下标的映射。
    pattern_indices: Vec<usize>,
}

/// 编译后的混合多模式匹配器。
struct AgentBatchMatcher {
    /// 区分大小写的普通关键字。
    literal_sensitive: Option<AhoPatternGroup>,
    /// 仅包含 ASCII 的忽略大小写普通关键字，直接使用自动机 ASCII 折叠。
    literal_ascii_insensitive: Option<AhoPatternGroup>,
    /// 包含非 ASCII 字符的忽略大小写关键字，对整行做 Unicode 小写后使用自动机。
    literal_unicode_insensitive: Option<AhoPatternGroup>,
    /// 区分大小写的正则集合。
    regex_sensitive: Option<RegexPatternGroup>,
    /// 忽略大小写的正则集合。
    regex_insensitive: Option<RegexPatternGroup>,
}

impl AgentBatchMatcher {
    /// 编译全部普通关键字和正则；任一表达式无效时在读取日志前返回错误。
    fn compile(patterns: &[AgentSearchPattern]) -> Result<Self, String> {
        if patterns.is_empty() {
            return Err("批量搜索至少需要一个模式".to_string());
        }
        if patterns.len() > u64::BITS as usize {
            return Err("批量搜索模式数量不能超过 64".to_string());
        }
        if let Some(pattern) = patterns.iter().find(|pattern| pattern.query.is_empty()) {
            return Err(format!("搜索模式 {} 的表达式不能为空", pattern.pattern_id));
        }

        let literal_sensitive = build_aho_group(
            patterns,
            |pattern| !pattern.regex && pattern.case_sensitive,
            false,
            false,
        )?;
        let literal_ascii_insensitive = build_aho_group(
            patterns,
            |pattern| !pattern.regex && !pattern.case_sensitive && pattern.query.is_ascii(),
            true,
            false,
        )?;
        let literal_unicode_insensitive = build_aho_group(
            patterns,
            |pattern| !pattern.regex && !pattern.case_sensitive && !pattern.query.is_ascii(),
            false,
            true,
        )?;
        let regex_sensitive = build_regex_group(patterns, true)?;
        let regex_insensitive = build_regex_group(patterns, false)?;
        Ok(Self {
            literal_sensitive,
            literal_ascii_insensitive,
            literal_unicode_insensitive,
            regex_sensitive,
            regex_insensitive,
        })
    }

    /// 返回当前行的模式命中位图；工具上限为 20，因此一个 `u64` 可消除逐行堆分配。
    fn matching_pattern_mask(&self, line: &str) -> u64 {
        let mut matched = 0_u64;
        mark_aho_matches(self.literal_sensitive.as_ref(), line, &mut matched);
        mark_aho_matches(self.literal_ascii_insensitive.as_ref(), line, &mut matched);
        if let Some(group) = self.literal_unicode_insensitive.as_ref() {
            let lowercase = line.to_lowercase();
            mark_aho_matches(Some(group), &lowercase, &mut matched);
        }
        mark_regex_matches(self.regex_sensitive.as_ref(), line, &mut matched);
        mark_regex_matches(self.regex_insensitive.as_ref(), line, &mut matched);
        matched
    }
}

/// 构造一个 Aho-Corasick 分组；Unicode 忽略大小写分组会在编译前统一小写。
fn build_aho_group(
    patterns: &[AgentSearchPattern],
    predicate: impl Fn(&AgentSearchPattern) -> bool,
    ascii_case_insensitive: bool,
    unicode_lowercase: bool,
) -> Result<Option<AhoPatternGroup>, String> {
    let selected = patterns
        .iter()
        .enumerate()
        .filter(|(_, pattern)| predicate(pattern))
        .map(|(index, pattern)| {
            let query = if unicode_lowercase {
                pattern.query.to_lowercase()
            } else {
                pattern.query.clone()
            };
            (index, query)
        })
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Ok(None);
    }
    let pattern_indices = selected.iter().map(|(index, _)| *index).collect();
    let queries = selected.iter().map(|(_, query)| query.as_str());
    let matcher = AhoCorasickBuilder::new()
        .ascii_case_insensitive(ascii_case_insensitive)
        .build(queries)
        .map_err(|error| format!("普通关键字自动机编译失败：{error}"))?;
    Ok(Some(AhoPatternGroup {
        matcher,
        pattern_indices,
    }))
}

/// 构造一个大小写语义一致的 RegexSet。
fn build_regex_group(
    patterns: &[AgentSearchPattern],
    case_sensitive: bool,
) -> Result<Option<RegexPatternGroup>, String> {
    let selected = patterns
        .iter()
        .enumerate()
        .filter(|(_, pattern)| pattern.regex && pattern.case_sensitive == case_sensitive)
        .map(|(index, pattern)| (index, pattern.query.as_str()))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Ok(None);
    }
    let pattern_indices = selected.iter().map(|(index, _)| *index).collect();
    let expressions = selected.iter().map(|(_, expression)| *expression);
    let matcher = RegexSetBuilder::new(expressions)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|error| format!("正则集合编译失败：{error}"))?;
    Ok(Some(RegexPatternGroup {
        matcher,
        pattern_indices,
    }))
}

/// 把一个自动机分组的重叠命中写入共享位图，避免前缀模式互相遮蔽。
fn mark_aho_matches(group: Option<&AhoPatternGroup>, line: &str, matched: &mut u64) {
    let Some(group) = group else {
        return;
    };
    for result in group.matcher.find_overlapping_iter(line) {
        if let Some(index) = group.pattern_indices.get(result.pattern().as_usize()) {
            *matched |= 1_u64 << *index;
        }
    }
}

/// 把 RegexSet 返回的全部模式 ID 写入共享位图。
fn mark_regex_matches(group: Option<&RegexPatternGroup>, line: &str, matched: &mut u64) {
    let Some(group) = group else {
        return;
    };
    for pattern_id in group.matcher.matches(line).iter() {
        if let Some(index) = group.pattern_indices.get(pattern_id) {
            *matched |= 1_u64 << *index;
        }
    }
}

/// 一个物理归档文件的分组键。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ArchiveRootKey {
    /// 最外层归档真实路径。
    archive_path: PathBuf,
    /// 最外层归档格式。
    root_format: ArchiveFormat,
}

/// 单个物理或嵌套容器的访问计划。
#[derive(Debug)]
struct ArchiveContainerPlan {
    /// 当前容器格式。
    format: ArchiveFormat,
    /// 当前容器内直接作为日志读取的条目及其来源下标。
    direct_entries: HashMap<String, Vec<usize>>,
    /// 当前容器内仍需向下进入的嵌套容器。
    nested_containers: HashMap<String, ArchiveContainerPlan>,
}

impl ArchiveContainerPlan {
    /// 创建空容器计划。
    fn new(format: ArchiveFormat) -> Self {
        Self {
            format,
            direct_entries: HashMap::new(),
            nested_containers: HashMap::new(),
        }
    }

    /// 把一个归档来源插入对应嵌套链路；相同容器路径只创建一次计划节点。
    fn insert_source(
        &mut self,
        container_entries: &[String],
        entry_path: &str,
        source_index: usize,
    ) -> Result<(), String> {
        let Some((container_entry, remaining)) = container_entries.split_first() else {
            self.direct_entries
                .entry(normalize_archive_entry_path(entry_path))
                .or_default()
                .push(source_index);
            return Ok(());
        };
        let normalized_container = normalize_archive_entry_path(container_entry);
        let format = detect_archive_format_by_name(&normalized_container)
            .ok_or_else(|| format!("无法识别嵌套归档格式：{normalized_container}"))?;
        self.nested_containers
            .entry(normalized_container)
            .or_insert_with(|| Self::new(format))
            .insert_source(remaining, entry_path, source_index)
    }

    /// 返回当前容器必须在一次遍历中访问的直接日志和嵌套容器条目集合。
    fn target_entries(&self) -> HashSet<String> {
        self.direct_entries
            .keys()
            .chain(self.nested_containers.keys())
            .cloned()
            .collect()
    }
}

/// 独立批量搜索引擎入口。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AgentBatchSearchEngine;

impl AgentBatchSearchEngine {
    /// 仅编译并校验全部模式，不接触任何日志来源。
    pub(crate) fn validate_patterns(patterns: &[AgentSearchPattern]) -> Result<(), String> {
        AgentBatchMatcher::compile(patterns).map(|_| ())
    }

    /// 流式扫描本地文件和归档分组，不经过 `AgentLogDocument`，也不建立全文行索引。
    pub(crate) fn search(
        scope: &SourceScopeSnapshot,
        sources: &[SnapshotSource],
        patterns: &[AgentSearchPattern],
        max_hits_per_pattern: usize,
        cancel_flag: Arc<AtomicBool>,
    ) -> AgentBatchSearchOutput {
        let matcher = match AgentBatchMatcher::compile(patterns) {
            Ok(matcher) => matcher,
            Err(error) => {
                return AgentBatchSearchOutput {
                    patterns: vec![AgentBatchPatternResult::default(); patterns.len()],
                    summary: AgentBatchSearchSummary {
                        errors: vec![error],
                        ..AgentBatchSearchSummary::default()
                    },
                };
            }
        };
        let mut runtime = AgentBatchSearchRuntime {
            scope,
            sources,
            matcher,
            max_hits_per_pattern,
            cancel_flag,
            patterns: vec![AgentBatchPatternResult::default(); patterns.len()],
            summary: AgentBatchSearchSummary::default(),
        };
        runtime.run();
        AgentBatchSearchOutput {
            patterns: runtime.patterns,
            summary: runtime.summary,
        }
    }
}

/// 一次阻塞批量搜索的可变执行状态。
struct AgentBatchSearchRuntime<'a> {
    /// 不可变会话来源和编码/密码配置。
    scope: &'a SourceScopeSnapshot,
    /// 本次明确选中的来源切片。
    sources: &'a [SnapshotSource],
    /// 编译一次后复用的混合匹配器。
    matcher: AgentBatchMatcher,
    /// 每个模式最多保留的证据条数。
    max_hits_per_pattern: usize,
    /// 异步会话桥接来的取消标记。
    cancel_flag: Arc<AtomicBool>,
    /// 输入顺序对应的累计结果。
    patterns: Vec<AgentBatchPatternResult>,
    /// 全局扫描统计。
    summary: AgentBatchSearchSummary,
}

impl AgentBatchSearchRuntime<'_> {
    /// 按位置类型分组后执行本地流式扫描和归档单遍扫描。
    fn run(&mut self) {
        let mut local_sources = Vec::new();
        let mut archive_groups = Vec::<(ArchiveRootKey, ArchiveContainerPlan)>::new();
        let mut archive_group_indices = HashMap::<ArchiveRootKey, usize>::new();

        for (source_index, source) in self.sources.iter().enumerate() {
            match &source.location {
                SourceLocation::LocalPath(_) => local_sources.push(source_index),
                SourceLocation::ArchiveEntry {
                    archive_path,
                    root_format,
                    container_entries,
                    entry_path,
                    ..
                } => {
                    let key = ArchiveRootKey {
                        archive_path: archive_path.clone(),
                        root_format: *root_format,
                    };
                    let group_index = match archive_group_indices.get(&key).copied() {
                        Some(index) => index,
                        None => {
                            let index = archive_groups.len();
                            archive_groups
                                .push((key.clone(), ArchiveContainerPlan::new(*root_format)));
                            archive_group_indices.insert(key, index);
                            index
                        }
                    };
                    if let Err(error) = archive_groups[group_index].1.insert_source(
                        container_entries,
                        entry_path,
                        source_index,
                    ) {
                        self.summary.errors.push(error);
                    }
                }
            }
        }

        for source_index in local_sources {
            if self.is_cancelled() {
                break;
            }
            self.scan_local_source(source_index);
        }
        for (key, plan) in archive_groups {
            if self.is_cancelled() {
                break;
            }
            self.visit_root_container(&key, &plan);
        }
        self.summary.was_cancelled |= self.cancel_flag.load(Ordering::Relaxed);
    }

    /// 打开一个本地日志并直接流式解码，不创建 mmap、全文 String 或行范围表。
    fn scan_local_source(&mut self, source_index: usize) {
        let source = &self.sources[source_index];
        self.summary.scanned_files = self.summary.scanned_files.saturating_add(1);
        let SourceLocation::LocalPath(path) = &source.location else {
            return;
        };
        match File::open(path)
            .with_context(|| format!("无法打开 Agent 批量搜索来源：{}", source.file_name))
            .and_then(|mut file| self.scan_source_reader(source_index, &mut file))
        {
            Ok(()) => {}
            Err(_error) if self.is_cancelled() => self.summary.was_cancelled = true,
            Err(error) => self.summary.errors.push(error.to_string()),
        }
    }

    /// 打开一次物理归档根，并执行其整棵目标容器计划。
    fn visit_root_container(&mut self, key: &ArchiveRootKey, plan: &ArchiveContainerPlan) {
        self.summary.opened_containers = self.summary.opened_containers.saturating_add(1);
        let Some(adapter) = archive_registry().adapter_for(plan.format) else {
            self.summary
                .errors
                .push(format!("当前不支持批量读取 {}", plan.format.label()));
            return;
        };
        let targets = plan.target_entries();
        let password_key = ArchivePasswordKey::root(key.archive_path.clone());
        let source_label = key.archive_path.display().to_string();
        let password = self.scope.archive_passwords.get(&password_key);
        let mut visited = HashSet::with_capacity(targets.len());
        let result = adapter.visit_entries(
            &key.archive_path,
            &targets,
            password,
            &mut |entry_path, reader| {
                visited.insert(normalize_archive_entry_path(entry_path));
                self.consume_container_entry(key, plan, &[], entry_path, reader)
            },
        );
        if let Err(error) = result {
            if self.is_cancelled() {
                self.summary.was_cancelled = true;
            } else {
                self.summary.errors.push(
                    annotate_archive_password_error(error, password_key, &source_label).to_string(),
                );
            }
        }
        self.record_missing_entries(plan, &visited, &source_label);
    }

    /// 在已经物化的嵌套容器上单次遍历全部目标条目。
    fn visit_nested_container(
        &mut self,
        root: &ArchiveRootKey,
        plan: &ArchiveContainerPlan,
        container_chain: &[String],
        bytes: Vec<u8>,
    ) {
        if self.is_cancelled() {
            self.summary.was_cancelled = true;
            return;
        }
        self.summary.opened_containers = self.summary.opened_containers.saturating_add(1);
        let Some(adapter) = archive_registry().adapter_for(plan.format) else {
            self.summary
                .errors
                .push(format!("当前不支持批量读取 {}", plan.format.label()));
            return;
        };
        let targets = plan.target_entries();
        let password_key = ArchivePasswordKey::new(root.archive_path.clone(), container_chain);
        let source_label = format!(
            "{}!/{}",
            root.archive_path.display(),
            container_chain.join("!/")
        );
        let password = self.scope.archive_passwords.get(&password_key);
        let reader_len = bytes.len() as u64;
        let mut reader = Cursor::new(bytes);
        let mut visited = HashSet::with_capacity(targets.len());
        let result = adapter.visit_entries_from_reader(
            &mut reader,
            reader_len,
            &targets,
            &source_label,
            password,
            &mut |entry_path, entry_reader| {
                visited.insert(normalize_archive_entry_path(entry_path));
                self.consume_container_entry(root, plan, container_chain, entry_path, entry_reader)
            },
        );
        if let Err(error) = result {
            if self.is_cancelled() {
                self.summary.was_cancelled = true;
            } else {
                self.summary.errors.push(
                    annotate_archive_password_error(error, password_key, source_label.clone())
                        .to_string(),
                );
            }
        }
        self.record_missing_entries(plan, &visited, &source_label);
    }

    /// 消费一个容器条目；普通日志直接搜索，嵌套容器只物化一次后递归。
    fn consume_container_entry(
        &mut self,
        root: &ArchiveRootKey,
        plan: &ArchiveContainerPlan,
        container_chain: &[String],
        entry_path: &str,
        reader: &mut dyn Read,
    ) -> Result<()> {
        if self.is_cancelled() {
            bail!("Agent 批量搜索已取消");
        }
        let normalized_path = normalize_archive_entry_path(entry_path);
        if let Some(source_indices) = plan.direct_entries.get(&normalized_path) {
            self.summary.scanned_files = self
                .summary
                .scanned_files
                .saturating_add(source_indices.len());
            if let Err(error) = self.scan_source_readers(source_indices, reader) {
                if self.is_cancelled() {
                    return Err(error);
                }
                self.summary.errors.push(error.to_string());
            }
            return Ok(());
        }
        let Some(nested_plan) = plan.nested_containers.get(&normalized_path) else {
            return Ok(());
        };
        let bytes = read_nested_container_bytes(reader, self.cancel_flag.as_ref())?;
        let mut nested_chain = container_chain.to_vec();
        nested_chain.push(normalized_path);
        self.visit_nested_container(root, nested_plan, &nested_chain, bytes);
        Ok(())
    }

    /// 流式解码一个日志读取器，并在每行到达时执行一次混合匹配。
    fn scan_source_reader(&mut self, source_index: usize, reader: &mut dyn Read) -> Result<()> {
        self.scan_source_readers(&[source_index], reader)
    }

    /// 单次读取一个物理日志，并把命中同步映射到可能重复加载的多个逻辑来源引用。
    fn scan_source_readers(
        &mut self,
        source_indices: &[usize],
        reader: &mut dyn Read,
    ) -> Result<()> {
        let source_file_name = source_indices
            .first()
            .and_then(|index| self.sources.get(*index))
            .map(|source| source.file_name.clone())
            .ok_or_else(|| anyhow!("Agent 批量搜索来源为空"))?;
        let mut decoder = StreamingLogDecoder::new(&self.scope.default_encoding);
        let mut buffer = [0_u8; BATCH_SEARCH_READ_CHUNK_BYTES];
        loop {
            if self.is_cancelled() {
                bail!("Agent 批量搜索已取消");
            }
            let read_count = reader
                .read(&mut buffer)
                .with_context(|| format!("无法读取 Agent 日志：{source_file_name}"))?;
            if read_count == 0 {
                decoder.finish(|line_number, line| {
                    for source_index in source_indices {
                        record_matching_line(
                            &self.matcher,
                            &mut self.patterns,
                            &mut self.summary,
                            &self.sources[*source_index],
                            self.max_hits_per_pattern,
                            line_number,
                            line,
                        );
                    }
                })?;
                return Ok(());
            }
            self.summary.scanned_bytes =
                self.summary.scanned_bytes.saturating_add(read_count as u64);
            decoder.push(&buffer[..read_count], |line_number, line| {
                for source_index in source_indices {
                    record_matching_line(
                        &self.matcher,
                        &mut self.patterns,
                        &mut self.summary,
                        &self.sources[*source_index],
                        self.max_hits_per_pattern,
                        line_number,
                        line,
                    );
                }
            })?;
        }
    }

    /// 对当前容器没有回调到的目标条目记录明确错误，便于识别归档内容变化。
    fn record_missing_entries(
        &mut self,
        plan: &ArchiveContainerPlan,
        visited: &HashSet<String>,
        source_label: &str,
    ) {
        if self.is_cancelled() {
            return;
        }
        for target in plan.target_entries().difference(visited) {
            self.summary
                .errors
                .push(format!("归档条目已不存在：{source_label}!/{target}"));
        }
    }

    /// 返回当前阻塞搜索是否已经收到取消信号。
    fn is_cancelled(&self) -> bool {
        self.cancel_flag.load(Ordering::Relaxed)
    }
}

/// 对一行执行一次混合匹配，并只为尚有证据槽位的模式复制正文。
fn record_matching_line(
    matcher: &AgentBatchMatcher,
    patterns: &mut [AgentBatchPatternResult],
    summary: &mut AgentBatchSearchSummary,
    source: &SnapshotSource,
    max_hits_per_pattern: usize,
    line_number: usize,
    line: &str,
) {
    summary.scanned_lines = summary.scanned_lines.saturating_add(1);
    let mut matched_mask = matcher.matching_pattern_mask(line);
    if matched_mask == 0 {
        return;
    }
    summary.matched_results = summary.matched_results.saturating_add(1);
    while matched_mask != 0 {
        let pattern_index = matched_mask.trailing_zeros() as usize;
        matched_mask &= matched_mask - 1;
        let Some(pattern) = patterns.get_mut(pattern_index) else {
            continue;
        };
        pattern.matched_lines = pattern.matched_lines.saturating_add(1);
        if pattern.hits.len() >= max_hits_per_pattern {
            continue;
        }
        pattern.hits.push(AgentBatchSearchHit {
            source_ref: source.source_ref.clone(),
            relative_path: source.relative_path.clone(),
            line_number,
            line_text: line.to_string(),
        });
    }
}

/// 以固定块读取一个嵌套容器，确保大容器物化期间仍能及时响应取消。
fn read_nested_container_bytes(reader: &mut dyn Read, cancel_flag: &AtomicBool) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; BATCH_SEARCH_READ_CHUNK_BYTES];
    loop {
        if cancel_flag.load(Ordering::Relaxed) {
            bail!("Agent 嵌套归档读取已取消");
        }
        let read_count = reader.read(&mut buffer)?;
        if read_count == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&buffer[..read_count]);
    }
}

/// 单日志增量解码器；只保留编码样本、当前未完成行和当前解码块。
struct StreamingLogDecoder {
    /// 用户配置的兜底编码标签。
    preferred_encoding: String,
    /// 编码确定前的有界头部样本。
    sample: Vec<u8>,
    /// 编码确定后的增量解码器。
    decoder: Option<encoding_rs::Decoder>,
    /// 尚未遇到换行符的 UTF-8 文本。
    pending_text: String,
    /// 下一条完整行的 0 基行号。
    next_line_number: usize,
}

impl StreamingLogDecoder {
    /// 创建尚未决定编码的流式解码器。
    fn new(preferred_encoding: &str) -> Self {
        Self {
            preferred_encoding: preferred_encoding.to_string(),
            sample: Vec::with_capacity(BATCH_SEARCH_ENCODING_SAMPLE_BYTES),
            decoder: None,
            pending_text: String::new(),
            next_line_number: 0,
        }
    }

    /// 推入一块原始字节；达到样本上限后立即开始增量解码和逐行回调。
    fn push(&mut self, mut bytes: &[u8], mut line_consumer: impl FnMut(usize, &str)) -> Result<()> {
        if self.decoder.is_none() {
            let remaining_sample =
                BATCH_SEARCH_ENCODING_SAMPLE_BYTES.saturating_sub(self.sample.len());
            let sampled = remaining_sample.min(bytes.len());
            self.sample.extend_from_slice(&bytes[..sampled]);
            bytes = &bytes[sampled..];
            if self.sample.len() < BATCH_SEARCH_ENCODING_SAMPLE_BYTES && bytes.is_empty() {
                return Ok(());
            }
            self.initialize_decoder(false, &mut line_consumer)?;
        }
        if !bytes.is_empty() {
            self.decode_bytes(bytes, false, &mut line_consumer)?;
        }
        Ok(())
    }

    /// 完成流；小日志此时才决定编码，并把最后一个无换行结尾的逻辑行交给调用方。
    fn finish(&mut self, mut line_consumer: impl FnMut(usize, &str)) -> Result<()> {
        if self.decoder.is_none() {
            self.initialize_decoder(true, &mut line_consumer)?;
        } else {
            self.decode_bytes(&[], true, &mut line_consumer)?;
        }
        if !self.pending_text.is_empty() {
            let line = self
                .pending_text
                .strip_suffix('\r')
                .unwrap_or(&self.pending_text);
            line_consumer(self.next_line_number, line);
            self.next_line_number = self.next_line_number.saturating_add(1);
            self.pending_text.clear();
        }
        Ok(())
    }

    /// 根据 BOM、UTF-8 合法性和 chardetng 样本创建增量解码器，并消费已缓存样本。
    fn initialize_decoder(
        &mut self,
        last: bool,
        line_consumer: &mut impl FnMut(usize, &str),
    ) -> Result<()> {
        let encoding = detect_stream_encoding(&self.sample, &self.preferred_encoding);
        self.decoder = Some(encoding.new_decoder_with_bom_removal());
        let sample = std::mem::take(&mut self.sample);
        self.decode_bytes(&sample, last, line_consumer)
    }

    /// 使用 encoding_rs 增量解码一块数据，并把新增 UTF-8 文本切成完整行。
    fn decode_bytes(
        &mut self,
        mut bytes: &[u8],
        last: bool,
        line_consumer: &mut impl FnMut(usize, &str),
    ) -> Result<()> {
        let decoder = self
            .decoder
            .as_mut()
            .ok_or_else(|| anyhow!("Agent 流式日志解码器尚未初始化"))?;
        loop {
            let capacity = decoder
                .max_utf8_buffer_length(bytes.len())
                .unwrap_or_else(|| bytes.len().saturating_mul(3).saturating_add(16))
                .max(16);
            let mut decoded = String::with_capacity(capacity);
            let (result, read, _had_errors) = decoder.decode_to_string(bytes, &mut decoded, last);
            bytes = &bytes[read..];
            self.pending_text.push_str(&decoded);
            emit_complete_lines(
                &mut self.pending_text,
                &mut self.next_line_number,
                line_consumer,
            );
            match result {
                CoderResult::InputEmpty => return Ok(()),
                CoderResult::OutputFull if read == 0 && decoded.is_empty() => {
                    bail!("Agent 日志解码器无法推进")
                }
                CoderResult::OutputFull => {}
            }
        }
    }
}

/// 识别流式日志编码；UTF-8 样本末尾的不完整字符不会被误判为其他编码。
fn detect_stream_encoding(sample: &[u8], preferred_encoding: &str) -> &'static Encoding {
    if sample.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return UTF_8;
    }
    if sample.starts_with(&[0xFF, 0xFE]) {
        return UTF_16LE;
    }
    if sample.starts_with(&[0xFE, 0xFF]) {
        return UTF_16BE;
    }
    match std::str::from_utf8(sample) {
        Ok(_) => return UTF_8,
        Err(error) if error.error_len().is_none() => return UTF_8,
        Err(_) => {}
    }
    let mut detector = EncodingDetector::new(Iso2022JpDetection::Deny);
    detector.feed(sample, true);
    let detected = detector.guess(None, Utf8Detection::Allow);
    if detected == UTF_8 {
        Encoding::for_label(preferred_encoding.trim().as_bytes()).unwrap_or(detected)
    } else {
        detected
    }
}

/// 从累计 UTF-8 文本中一次性发出全部完整行，最后只移动一次剩余尾部。
fn emit_complete_lines(
    pending_text: &mut String,
    next_line_number: &mut usize,
    line_consumer: &mut impl FnMut(usize, &str),
) {
    let mut line_start = 0usize;
    for (offset, byte) in pending_text.as_bytes().iter().copied().enumerate() {
        if byte != b'\n' {
            continue;
        }
        let line = pending_text[line_start..offset]
            .strip_suffix('\r')
            .unwrap_or(&pending_text[line_start..offset]);
        line_consumer(*next_line_number, line);
        *next_line_number = next_line_number.saturating_add(1);
        line_start = offset + 1;
    }
    if line_start > 0 {
        pending_text.drain(..line_start);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use sevenz_rust::{SevenZArchiveEntry, SevenZWriter};
    use tempfile::TempDir;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    use super::*;
    use crate::config::paths::temporary_test_dir;
    use crate::loader::SourceId;
    use crate::loader::archive::ArchivePasswordStore;

    /// 构造仅供独立批量搜索使用的来源快照。
    fn test_scope(sources: Vec<SnapshotSource>) -> SourceScopeSnapshot {
        SourceScopeSnapshot {
            session_id: "batch-search-test".to_string(),
            root_label: "test-root".to_string(),
            sources: Arc::new(sources),
            profiles: Arc::new(HashMap::new()),
            default_encoding: "UTF-8".to_string(),
            archive_passwords: ArchivePasswordStore::default(),
            allow_raw_log_content: true,
        }
    }

    /// 构造一个归档日志来源，测试只关心位置、引用与展示路径。
    fn archive_source(
        source_id: usize,
        archive_path: PathBuf,
        root_format: ArchiveFormat,
        container_entries: Vec<String>,
        entry_path: &str,
        format: ArchiveFormat,
    ) -> SnapshotSource {
        SnapshotSource {
            source_ref: format!("source-{source_id}"),
            source_id: SourceId(source_id),
            file_name: entry_path
                .rsplit('/')
                .next()
                .unwrap_or(entry_path)
                .to_string(),
            relative_path: entry_path.to_string(),
            profile_match_path: entry_path.to_string(),
            location: SourceLocation::ArchiveEntry {
                archive_path,
                root_format,
                container_entries,
                entry_path: entry_path.to_string(),
                format,
                archive_depth: source_id,
            },
            size: None,
            profile_id: None,
        }
    }

    /// 返回覆盖普通关键字和正则的固定测试模式。
    fn test_patterns() -> Vec<AgentSearchPattern> {
        vec![
            AgentSearchPattern {
                pattern_id: "error".to_string(),
                query: "ERROR".to_string(),
                case_sensitive: false,
                regex: false,
            },
            AgentSearchPattern {
                pattern_id: "code".to_string(),
                query: r"code=\d+".to_string(),
                case_sensitive: true,
                regex: true,
            },
        ]
    }

    /// 执行固定双日志归档断言，验证计数、行号和容器打开次数。
    fn assert_two_entry_archive(scope: SourceScopeSnapshot) {
        let output = AgentBatchSearchEngine::search(
            &scope,
            scope.sources.as_ref(),
            &test_patterns(),
            10,
            Arc::new(AtomicBool::new(false)),
        );
        assert!(
            output.summary.errors.is_empty(),
            "{:?}",
            output.summary.errors
        );
        assert_eq!(output.summary.opened_containers, 1);
        assert_eq!(output.summary.scanned_files, 2);
        assert_eq!(output.patterns[0].matched_lines, 2);
        assert_eq!(output.patterns[1].matched_lines, 1);
        assert_eq!(output.patterns[0].hits[0].line_number, 0);
    }

    /// 验证普通关键字前缀重叠不会互相遮蔽，正则集合也能同时返回全部命中。
    #[test]
    fn mixed_matcher_reports_all_overlapping_patterns() {
        let matcher = AgentBatchMatcher::compile(&[
            AgentSearchPattern {
                pattern_id: "short".to_string(),
                query: "error".to_string(),
                case_sensitive: false,
                regex: false,
            },
            AgentSearchPattern {
                pattern_id: "long".to_string(),
                query: "error code".to_string(),
                case_sensitive: false,
                regex: false,
            },
            AgentSearchPattern {
                pattern_id: "regex".to_string(),
                query: r"code\s+\d+".to_string(),
                case_sensitive: false,
                regex: true,
            },
        ])
        .expect("混合模式应成功编译");
        assert_eq!(matcher.matching_pattern_mask("ERROR code 500"), 0b111);
    }

    /// 验证分块解码不会把跨块 UTF-8 字符或跨块行拆坏。
    #[test]
    fn streaming_decoder_preserves_split_utf8_lines() {
        let text = "第一行 ERROR\n第二行正常\n末行";
        let bytes = text.as_bytes();
        let mut decoder = StreamingLogDecoder::new("UTF-8");
        let mut lines = Vec::new();
        for chunk in bytes.chunks(3) {
            decoder
                .push(chunk, |line_number, line| {
                    lines.push((line_number, line.to_string()))
                })
                .expect("分块解码应成功");
        }
        decoder
            .finish(|line_number, line| lines.push((line_number, line.to_string())))
            .expect("结束解码应成功");
        assert_eq!(
            lines,
            vec![
                (0, "第一行 ERROR".to_string()),
                (1, "第二行正常".to_string()),
                (2, "末行".to_string()),
            ]
        );
    }

    /// 验证同一 ZIP 中的多条日志只创建一个容器读取器。
    #[test]
    fn zip_entries_are_searched_with_one_container_open() {
        let directory = temporary_test_dir("agent-batch-zip");
        let archive_path = directory.path().join("logs.zip");
        let mut writer = ZipWriter::new(File::create(&archive_path).expect("应创建 ZIP 测试归档"));
        writer
            .start_file("a.log", SimpleFileOptions::default())
            .expect("应创建首个 ZIP 日志");
        writer
            .write_all(b"ERROR code=500\nnormal\n")
            .expect("应写入首个 ZIP 日志");
        writer
            .start_file("nested/b.log", SimpleFileOptions::default())
            .expect("应创建第二个 ZIP 日志");
        writer
            .write_all(b"ERROR timeout\n")
            .expect("应写入第二个 ZIP 日志");
        writer.finish().expect("应完成 ZIP 测试归档");

        assert_two_entry_archive(test_scope(vec![
            archive_source(
                1,
                archive_path.clone(),
                ArchiveFormat::Zip,
                Vec::new(),
                "a.log",
                ArchiveFormat::Zip,
            ),
            archive_source(
                2,
                archive_path,
                ArchiveFormat::Zip,
                Vec::new(),
                "nested/b.log",
                ArchiveFormat::Zip,
            ),
        ]));
    }

    /// 验证顺序 TAR 只遍历一次即可读取多个目标条目。
    #[test]
    fn tar_entries_are_searched_with_one_container_open() {
        let directory = temporary_test_dir("agent-batch-tar");
        let archive_path = directory.path().join("logs.tar");
        let file = File::create(&archive_path).expect("应创建 TAR 测试归档");
        let mut writer = tar::Builder::new(file);
        append_tar_entry(&mut writer, "a.log", b"ERROR code=500\nnormal\n");
        append_tar_entry(&mut writer, "nested/b.log", b"ERROR timeout\n");
        writer.finish().expect("应完成 TAR 测试归档");

        assert_two_entry_archive(test_scope(vec![
            archive_source(
                1,
                archive_path.clone(),
                ArchiveFormat::Tar,
                Vec::new(),
                "a.log",
                ArchiveFormat::Tar,
            ),
            archive_source(
                2,
                archive_path,
                ArchiveFormat::Tar,
                Vec::new(),
                "nested/b.log",
                ArchiveFormat::Tar,
            ),
        ]));
    }

    /// 验证 7Z solid-folder 通过一次 for_each_entries 扫描多个目标日志。
    #[test]
    fn sevenz_entries_are_searched_with_one_container_open() {
        let directory = temporary_test_dir("agent-batch-7z");
        let archive_path = directory.path().join("logs.7z");
        let first_path = write_test_file(&directory, "first.log", b"ERROR code=500\nnormal\n");
        let second_path = write_test_file(&directory, "second.log", b"ERROR timeout\n");
        let mut writer = SevenZWriter::create(&archive_path).expect("应创建 7Z 测试归档");
        writer
            .push_archive_entry(
                SevenZArchiveEntry::from_path(&first_path, "a.log".to_string()),
                Some(File::open(&first_path).expect("应打开首个 7Z 输入")),
            )
            .expect("应写入首个 7Z 日志");
        writer
            .push_archive_entry(
                SevenZArchiveEntry::from_path(&second_path, "nested/b.log".to_string()),
                Some(File::open(&second_path).expect("应打开第二个 7Z 输入")),
            )
            .expect("应写入第二个 7Z 日志");
        writer.finish().expect("应完成 7Z 测试归档");

        assert_two_entry_archive(test_scope(vec![
            archive_source(
                1,
                archive_path.clone(),
                ArchiveFormat::SevenZ,
                Vec::new(),
                "a.log",
                ArchiveFormat::SevenZ,
            ),
            archive_source(
                2,
                archive_path,
                ArchiveFormat::SevenZ,
                Vec::new(),
                "nested/b.log",
                ArchiveFormat::SevenZ,
            ),
        ]));
    }

    /// 验证外层和内层 ZIP 各自只打开一次，内层多个日志不会重复解压 inner.zip。
    #[test]
    fn nested_container_is_materialized_and_opened_once() {
        let directory = temporary_test_dir("agent-batch-nested");
        let mut inner_cursor = Cursor::new(Vec::new());
        {
            let mut inner_writer = ZipWriter::new(&mut inner_cursor);
            inner_writer
                .start_file("a.log", SimpleFileOptions::default())
                .expect("应创建内层首个日志");
            inner_writer
                .write_all(b"ERROR code=500\n")
                .expect("应写入内层首个日志");
            inner_writer
                .start_file("b.log", SimpleFileOptions::default())
                .expect("应创建内层第二个日志");
            inner_writer
                .write_all(b"ERROR timeout\n")
                .expect("应写入内层第二个日志");
            inner_writer.finish().expect("应完成内层 ZIP");
        }
        let archive_path = directory.path().join("outer.zip");
        let mut outer_writer = ZipWriter::new(File::create(&archive_path).expect("应创建外层 ZIP"));
        outer_writer
            .start_file("inner.zip", SimpleFileOptions::default())
            .expect("应创建嵌套容器条目");
        outer_writer
            .write_all(inner_cursor.get_ref())
            .expect("应写入嵌套容器");
        outer_writer.finish().expect("应完成外层 ZIP");

        let scope = test_scope(vec![
            archive_source(
                1,
                archive_path.clone(),
                ArchiveFormat::Zip,
                vec!["inner.zip".to_string()],
                "a.log",
                ArchiveFormat::Zip,
            ),
            archive_source(
                2,
                archive_path,
                ArchiveFormat::Zip,
                vec!["inner.zip".to_string()],
                "b.log",
                ArchiveFormat::Zip,
            ),
        ]);
        let output = AgentBatchSearchEngine::search(
            &scope,
            scope.sources.as_ref(),
            &test_patterns(),
            10,
            Arc::new(AtomicBool::new(false)),
        );
        assert!(
            output.summary.errors.is_empty(),
            "{:?}",
            output.summary.errors
        );
        assert_eq!(output.summary.opened_containers, 2);
        assert_eq!(output.summary.scanned_files, 2);
        assert_eq!(output.patterns[0].matched_lines, 2);
    }

    /// 向 TAR 写入一个普通文件条目。
    fn append_tar_entry(writer: &mut tar::Builder<File>, path: &str, content: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        writer
            .append_data(&mut header, path, content)
            .expect("应写入 TAR 日志条目");
    }

    /// 写入 7Z 构造器需要的隔离输入文件。
    fn write_test_file(directory: &TempDir, name: &str, content: &[u8]) -> PathBuf {
        let path = directory.path().join(name);
        std::fs::write(&path, content).expect("应写入 7Z 输入文件");
        path
    }
}
