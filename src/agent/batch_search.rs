//! 文件职责：为 AI Agent 提供独立的流式多模式批量日志搜索引擎。
//! 创建日期：2026-07-17
//! 修改日期：2026-07-17
//! 作者：Argus 开发团队
//! 主要功能：流式解码日志、使用 Aho-Corasick/RegexSet 一次匹配多模式。

use std::fs::File;
use std::io::Read;
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
    /// 单个来源失败后保留的非致命错误。
    pub errors: Vec<String>,
    /// 是否在读取块边界响应用户取消。
    pub was_cancelled: bool,
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
    /// 逐个流式扫描本地来源。
    fn run(&mut self) {
        let mut local_sources = Vec::new();

        for (source_index, source) in self.sources.iter().enumerate() {
            match &source.location {
                SourceLocation::LocalPath(_) => local_sources.push(source_index),
            }
        }

        for source_index in local_sources {
            if self.is_cancelled() {
                break;
            }
            self.scan_local_source(source_index);
        }
        self.summary.was_cancelled |= self.cancel_flag.load(Ordering::Relaxed);
    }

    /// 打开一个本地日志并直接流式解码，不创建 mmap、全文 String 或行范围表。
    fn scan_local_source(&mut self, source_index: usize) {
        let source = &self.sources[source_index];
        self.summary.scanned_files = self.summary.scanned_files.saturating_add(1);
        let SourceLocation::LocalPath(path) = &source.location;
        match File::open(path)
            .with_context(|| format!("无法打开 Agent 批量搜索来源：{}", source.file_name))
            .and_then(|mut file| self.scan_source_reader(source_index, &mut file))
        {
            Ok(()) => {}
            Err(_error) if self.is_cancelled() => self.summary.was_cancelled = true,
            Err(error) => self.summary.errors.push(error.to_string()),
        }
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
    use super::*;

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
}
