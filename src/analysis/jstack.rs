//! 文件职责：实现 Jstack 线程日志解析、聚合和读取入口。
//! 创建日期：2026-06-16
//! 修改日期：2026-06-25
//! 作者：Argus 开发团队
//! 主要功能：把多个线程栈日志快照聚合为线程频率矩阵，供主内容区分析页签渲染。

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;

use crate::config::LoaderConfig;
use crate::loader::archive::{ArchivePasswordStore, detect_archive_format};
use crate::loader::{SourceId, SourceKind, SourceLocation, SourceMetadata, SourceTreeNode};
use crate::reader::log_file_reader::{LogDocument, LogFileReader, OpenLogRequest};

use super::source_input::{collect_analysis_files, resolve_analysis_location};

/// Jstack 本地目录递归时识别的普通文本日志扩展名。
const JSTACK_TEXT_EXTENSIONS: &[&str] = &["log", "txt", "out", "dump"];

/// Jstack 线程状态，聚合 UI 会按该枚举映射颜色。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum JstackThreadState {
    /// Java 线程正在运行或可运行。
    Runnable,
    /// Java 线程阻塞在监视器或锁上。
    Blocked,
    /// Java 线程无限期等待。
    Waiting,
    /// Java 线程限时等待。
    TimedWaiting,
    /// 未识别或缺失状态。
    Other,
}

impl JstackThreadState {
    /// 返回 UI 展示用状态标签。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Runnable => "RUNNABLE",
            Self::Blocked => "BLOCKED",
            Self::Waiting => "WAITING",
            Self::TimedWaiting => "TIMED_WAITING",
            Self::Other => "OTHER",
        }
    }

    /// 解析 `java.lang.Thread.State:` 后面的状态值。
    fn from_state_text(text: &str) -> Self {
        let state = text
            .trim()
            .split(|character: char| character.is_ascii_whitespace() || character == '(')
            .next()
            .unwrap_or_default();
        match state {
            "RUNNABLE" => Self::Runnable,
            "BLOCKED" => Self::Blocked,
            "WAITING" => Self::Waiting,
            "TIMED_WAITING" => Self::TimedWaiting,
            _ => Self::Other,
        }
    }

    /// 返回平票时的状态优先级；数字越小越优先。
    fn tie_break_priority(self) -> usize {
        match self {
            Self::Blocked => 0,
            Self::Runnable => 1,
            Self::TimedWaiting => 2,
            Self::Waiting => 3,
            Self::Other => 4,
        }
    }
}

/// 单个快照中的线程样本。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JstackThreadSample {
    /// 线程名称，取自 jstack 线程头第一段双引号内容。
    pub thread_name: String,
    /// 线程 ID 摘要，优先包含 `#id`、`tid=` 和 `nid=`，用于区分同名线程。
    pub thread_id: String,
    /// 解析出的线程状态。
    pub state: JstackThreadState,
    /// 当前线程块的完整堆栈行，包含线程头、状态行和后续 `at ...` 明细。
    pub stack_lines: Arc<[String]>,
}

/// 线程在某个快照中的一次具体出现记录。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JstackThreadStackOccurrence {
    /// 快照序号，和 `JstackAnalysisResult.snapshots` 对齐。
    pub snapshot_index: usize,
    /// 快照展示名称。
    pub snapshot_label: String,
    /// 快照路径或压缩包虚拟路径。
    pub snapshot_path: String,
    /// 线程名称。
    pub thread_name: String,
    /// 线程 ID 摘要，和线程名共同构成线程身份。
    pub thread_id: String,
    /// 该次出现解析出的状态。
    pub state: JstackThreadState,
    /// 同一线程名在同一快照内的出现序号，从 1 开始。
    pub occurrence_index: usize,
    /// 当前线程块完整堆栈行。
    pub stack_lines: Arc<[String]>,
    /// 当前线程块用于过滤匹配的归一化文本，避免 UI 渲染期反复拼接和小写化堆栈。
    pub normalized_stack_text: Arc<str>,
}

/// 线程详情窗口所需的数据快照。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JstackThreadDetail {
    /// 被查看的线程名称。
    pub thread_name: String,
    /// 被查看线程的 ID 摘要。
    pub thread_id: String,
    /// 该线程跨多个快照的出现记录，保持快照输入顺序。
    pub occurrences: Vec<JstackThreadStackOccurrence>,
}

/// 一个被分析文件对应的 Jstack 快照。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JstackSnapshot {
    /// 来源树节点 ID。
    pub source_id: SourceId,
    /// 来源展示名称。
    pub label: String,
    /// 来源路径或压缩包虚拟路径。
    pub path: String,
    /// 该快照中解析到的线程样本。
    pub samples: Vec<JstackThreadSample>,
}

/// 分析任务输入目标。
#[derive(Clone, Debug)]
pub(crate) struct JstackAnalysisTarget {
    /// 来源树节点 ID。
    pub source_id: SourceId,
    /// 来源位置，可能是本地文件或压缩包内条目。
    pub location: SourceLocation,
    /// 待探测单文件压缩包节点快照；存在时分析后台会先独立探测真实日志条目。
    pub archive_probe_node: Option<SourceTreeNode>,
    /// UI 展示名称。
    pub label: String,
    /// 路径展示文本。
    pub path: String,
    /// 当前会话中已输入的压缩包密码快照。
    pub archive_passwords: ArchivePasswordStore,
}

/// 频率矩阵中单个线程在单个快照中的聚合格子。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JstackFrequencyCell {
    /// 快照序号，和 `JstackAnalysisResult.snapshots` 对齐。
    pub snapshot_index: usize,
    /// 出现次数；为 0 时表示该线程没有出现在当前快照。
    pub count: usize,
    /// 当前格子的主状态；没有出现时为 `None`。
    pub state: Option<JstackThreadState>,
    /// 当前线程在该快照中的全部堆栈出现记录。
    pub stack_occurrences: Vec<JstackThreadStackOccurrence>,
}

/// 频率矩阵中的线程行。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JstackFrequencyRow {
    /// 线程名称。
    pub thread_name: String,
    /// 线程 ID 摘要，避免同名不同线程被聚合到同一行。
    pub thread_id: String,
    /// 该线程在全部快照中的总出现次数。
    pub total_count: usize,
    /// 每个快照对应一个格子，顺序和快照列一致。
    pub cells: Vec<JstackFrequencyCell>,
}

impl JstackThreadDetail {
    /// 返回适合 UI 展示和复制的线程身份文本。
    pub(crate) fn display_label(&self) -> String {
        thread_display_label(&self.thread_name, &self.thread_id)
    }
}

impl JstackFrequencyRow {
    /// 返回矩阵行使用的线程身份文本。
    pub(crate) fn display_label(&self) -> String {
        thread_display_label(&self.thread_name, &self.thread_id)
    }
}

/// 被跳过或读取失败的快照。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JstackSkippedSnapshot {
    /// 来源树节点 ID。
    pub source_id: SourceId,
    /// 来源展示名称。
    pub label: String,
    /// 跳过原因。
    pub reason: String,
}

/// Jstack 分析结果，包含快照列、线程行和诊断统计。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JstackAnalysisResult {
    /// 成功解析的快照列。
    pub snapshots: Vec<JstackSnapshot>,
    /// 按总频率排序后的线程行。
    pub rows: Vec<JstackFrequencyRow>,
    /// 跳过或读取失败的来源列表。
    pub skipped_snapshots: Vec<JstackSkippedSnapshot>,
    /// 所有输入文件数量。
    pub total_files: usize,
    /// 解析到的线程样本总数。
    pub total_samples: usize,
}

/// Jstack 线程过滤器，按线程名关键字和完整线程段片段隐藏分析结果。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct JstackThreadFilter {
    /// 线程名匹配规则，已转为小写。
    thread_name_patterns: Vec<JstackThreadNamePattern>,
    /// 完整线程段匹配片段，已转为小写并处理转义换行。
    stack_segment_patterns: Vec<String>,
}

/// Jstack 线程名过滤规则，兼容旧版模糊匹配并扩展 `*` / `?` 通配符。
#[derive(Clone, Debug, Eq, PartialEq)]
enum JstackThreadNamePattern {
    /// 不含通配符的旧规则，按子串模糊匹配。
    Contains(String),
    /// 含 `*` 或 `?` 的规则，按完整线程名执行 glob 匹配。
    Wildcard(String),
}

impl JstackThreadFilter {
    /// 从设置页原始文本创建过滤器。
    ///
    /// 参数说明：
    /// - `thread_name_filters`：线程名关键字，支持逗号、分号、竖线和换行分隔。
    /// - `stack_segment_filters`：完整线程段片段，使用空行分隔多个片段；兼容旧版 `||` 分隔。
    ///
    /// 返回值：归一化后的过滤器；空白配置会被忽略。
    pub(crate) fn from_raw(thread_name_filters: &str, stack_segment_filters: &str) -> Self {
        Self {
            thread_name_patterns: parse_thread_name_filter_patterns(thread_name_filters),
            stack_segment_patterns: parse_stack_segment_filter_patterns(stack_segment_filters),
        }
    }

    /// 返回过滤器是否没有任何有效规则。
    pub(crate) fn is_empty(&self) -> bool {
        self.thread_name_patterns.is_empty() && self.stack_segment_patterns.is_empty()
    }

    /// 判断一个频率行是否命中配置过滤规则。
    pub(crate) fn matches_row(&self, row: &JstackFrequencyRow) -> bool {
        if self.is_empty() {
            return false;
        }

        self.matches_thread_name(&row.thread_name)
            || row.cells.iter().any(|cell| {
                cell.stack_occurrences
                    .iter()
                    .any(|occurrence| self.matches_stack_text(&occurrence.normalized_stack_text))
            })
    }

    /// 判断线程名是否命中任意模糊匹配关键字。
    fn matches_thread_name(&self, thread_name: &str) -> bool {
        if self.thread_name_patterns.is_empty() {
            return false;
        }

        let normalized_thread_name = thread_name.to_lowercase();
        self.thread_name_patterns
            .iter()
            .any(|pattern| match pattern {
                JstackThreadNamePattern::Contains(pattern) => {
                    normalized_thread_name.contains(pattern)
                }
                JstackThreadNamePattern::Wildcard(pattern) => {
                    wildcard_pattern_matches(pattern, &normalized_thread_name)
                }
            })
    }

    /// 判断已归一化的完整线程段是否包含任意配置片段。
    fn matches_stack_text(&self, normalized_stack_text: &str) -> bool {
        if self.stack_segment_patterns.is_empty() {
            return false;
        }

        self.stack_segment_patterns
            .iter()
            .any(|pattern| normalized_stack_text.contains(pattern))
    }
}

/// 增量 Jstack 解析器，避免分析大文件时先拼接完整日志文本。
#[derive(Debug)]
struct JstackSnapshotParser {
    /// 已完成解析的线程样本。
    samples: Vec<JstackThreadSample>,
    /// 当前正在收集的线程名和线程 ID。
    current_thread: Option<(String, String)>,
    /// 当前线程块解析到的状态。
    current_state: JstackThreadState,
    /// 当前线程块原始堆栈行。
    current_stack_lines: Vec<String>,
}

impl Default for JstackSnapshotParser {
    /// 创建空解析器，并把当前状态初始化为 `OTHER` 作为缺失状态的兜底值。
    fn default() -> Self {
        Self {
            samples: Vec::new(),
            current_thread: None,
            current_state: JstackThreadState::Other,
            current_stack_lines: Vec::new(),
        }
    }
}

/// 单个快照中某线程的聚合缓存。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct JstackSnapshotThreadAggregate {
    /// 按线程状态统计出现次数，用于选择格子主状态。
    state_counts: BTreeMap<JstackThreadState, usize>,
    /// 同一快照内该线程的全部堆栈块。
    stack_occurrences: Vec<JstackThreadStackOccurrence>,
}

impl JstackAnalysisResult {
    /// 返回成功快照数量。
    pub(crate) fn snapshot_count(&self) -> usize {
        self.snapshots.len()
    }

    /// 返回不同线程数量。
    pub(crate) fn thread_count(&self) -> usize {
        self.rows.len()
    }

    /// 返回跳过文件数量。
    pub(crate) fn skipped_count(&self) -> usize {
        self.skipped_snapshots.len()
    }
}

/// 从多个来源读取并分析 Jstack 快照。
///
/// 参数说明：
/// - `targets`：按来源树顺序排列的分析目标。
/// - `default_encoding`：日志读取兜底编码。
///
/// 返回值：可直接供 UI 渲染的频率矩阵结果。
pub(crate) fn analyze_jstack_targets(
    targets: Vec<JstackAnalysisTarget>,
    default_encoding: String,
    loader_config: LoaderConfig,
) -> JstackAnalysisResult {
    let mut snapshot_targets = Vec::new();
    let mut snapshots = Vec::new();
    let mut skipped_snapshots = Vec::new();

    for target in targets {
        match expand_jstack_target(target, &loader_config) {
            Ok(mut expanded) => snapshot_targets.append(&mut expanded),
            Err((source_id, label, reason)) => skipped_snapshots.push(JstackSkippedSnapshot {
                source_id,
                label,
                reason,
            }),
        }
    }

    let total_files = snapshot_targets.len();
    for target in snapshot_targets {
        match read_jstack_snapshot(target.clone(), &default_encoding, &loader_config) {
            Ok(snapshot) if snapshot.samples.is_empty() => {
                skipped_snapshots.push(JstackSkippedSnapshot {
                    source_id: target.source_id,
                    label: target.label,
                    reason: "未解析到 Jstack 线程".to_string(),
                });
            }
            Ok(snapshot) => snapshots.push(snapshot),
            Err(error) => skipped_snapshots.push(JstackSkippedSnapshot {
                source_id: target.source_id,
                label: target.label,
                reason: error.to_string(),
            }),
        }
    }

    build_analysis_result(snapshots, skipped_snapshots, total_files)
}

/// 展开 Jstack 分析目标；本地目录会递归转换为可读取的日志或单文件压缩包快照。
fn expand_jstack_target(
    target: JstackAnalysisTarget,
    loader_config: &LoaderConfig,
) -> std::result::Result<Vec<JstackAnalysisTarget>, (SourceId, String, String)> {
    let SourceLocation::LocalPath(path) = &target.location else {
        return Ok(vec![target]);
    };
    if !path.is_dir() {
        return Ok(vec![target]);
    }

    collect_jstack_log_files(target.source_id, path, loader_config).map_err(|error| {
        (
            target.source_id,
            target.label.clone(),
            format!("读取 Jstack 目录失败：{error}"),
        )
    })
}

/// 递归收集本地目录中的 Jstack 文本日志和可独立探测的压缩包文件。
fn collect_jstack_log_files(
    source_id: SourceId,
    root: &Path,
    loader_config: &LoaderConfig,
) -> Result<Vec<JstackAnalysisTarget>> {
    Ok(collect_analysis_files(
        root,
        loader_config.follow_symlinks,
        is_jstack_candidate_file,
    )?
    .into_iter()
    .filter_map(|path| jstack_target_for_local_file(source_id, path))
    .collect())
}

/// 判断本地文件是否值得作为 Jstack 快照尝试解析。
fn is_jstack_candidate_file(path: &Path) -> bool {
    if detect_archive_format(path).is_some() {
        return true;
    }

    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            let extension = extension.to_ascii_lowercase();
            JSTACK_TEXT_EXTENSIONS.contains(&extension.as_str())
        })
        .unwrap_or(false)
}

/// 把本地文件路径转换为 Jstack 分析目标；压缩包会附带单文件探测快照。
fn jstack_target_for_local_file(
    source_id: SourceId,
    path: PathBuf,
) -> Option<JstackAnalysisTarget> {
    let label = path.file_name()?.to_string_lossy().to_string();
    let archive_probe_node = detect_archive_format(&path).map(|format| SourceTreeNode {
        id: source_id,
        parent_id: None,
        depth: 0,
        label: label.clone(),
        kind: SourceKind::Archive(format),
        location: SourceLocation::LocalPath(path.clone()),
        metadata: SourceMetadata {
            size: fs::metadata(&path).ok().map(|metadata| metadata.len()),
            children_loaded: false,
            is_loading: false,
            message: None,
        },
        selected: false,
        expanded: false,
    });

    Some(JstackAnalysisTarget {
        source_id,
        location: SourceLocation::LocalPath(path.clone()),
        archive_probe_node,
        label,
        path: path.display().to_string(),
        archive_passwords: ArchivePasswordStore::default(),
    })
}

/// 解析单份 Jstack 文本。
///
/// 参数说明：
/// - `source_id`：来源树节点 ID。
/// - `label`：来源展示名称。
/// - `path`：路径展示文本。
/// - `text`：完整日志文本。
///
/// 返回值：一个快照的线程样本列表。
#[cfg(test)]
pub(crate) fn parse_jstack_snapshot(
    source_id: SourceId,
    label: impl Into<String>,
    path: impl Into<String>,
    text: &str,
) -> JstackSnapshot {
    let mut parser = JstackSnapshotParser::default();
    for line in text.lines() {
        parser.push_line(line);
    }

    JstackSnapshot {
        source_id,
        label: label.into(),
        path: path.into(),
        samples: parser.finish(),
    }
}

/// 由已解析快照构建频率矩阵。
pub(crate) fn build_analysis_result(
    snapshots: Vec<JstackSnapshot>,
    skipped_snapshots: Vec<JstackSkippedSnapshot>,
    total_files: usize,
) -> JstackAnalysisResult {
    let mut thread_identities = BTreeSet::new();
    let mut per_snapshot_threads: Vec<HashMap<(String, String), JstackSnapshotThreadAggregate>> =
        Vec::with_capacity(snapshots.len());
    let mut total_samples = 0_usize;

    for (snapshot_index, snapshot) in snapshots.iter().enumerate() {
        let mut threads = HashMap::<(String, String), JstackSnapshotThreadAggregate>::new();
        for sample in &snapshot.samples {
            let thread_identity = (sample.thread_name.clone(), sample.thread_id.clone());
            thread_identities.insert(thread_identity.clone());
            total_samples += 1;
            let aggregate = threads.entry(thread_identity).or_default();
            *aggregate.state_counts.entry(sample.state).or_default() += 1;
            let occurrence_index = aggregate.stack_occurrences.len() + 1;
            aggregate
                .stack_occurrences
                .push(JstackThreadStackOccurrence {
                    snapshot_index,
                    snapshot_label: snapshot.label.clone(),
                    snapshot_path: snapshot.path.clone(),
                    thread_name: sample.thread_name.clone(),
                    thread_id: sample.thread_id.clone(),
                    state: sample.state,
                    occurrence_index,
                    stack_lines: sample.stack_lines.clone(),
                    normalized_stack_text: normalized_stack_search_text(
                        sample.stack_lines.as_ref(),
                    ),
                });
        }
        per_snapshot_threads.push(threads);
    }

    let mut rows = thread_identities
        .into_iter()
        .map(|(thread_name, thread_id)| {
            let mut total_count = 0_usize;
            let cells = per_snapshot_threads
                .iter()
                .enumerate()
                .map(|(snapshot_index, threads)| {
                    let aggregate = threads.get(&(thread_name.clone(), thread_id.clone()));
                    let count = aggregate
                        .map(|aggregate| aggregate.stack_occurrences.len())
                        .unwrap_or_default();
                    total_count += count;
                    JstackFrequencyCell {
                        snapshot_index,
                        count,
                        state: aggregate
                            .and_then(|aggregate| dominant_state(&aggregate.state_counts)),
                        stack_occurrences: aggregate
                            .map(|aggregate| aggregate.stack_occurrences.clone())
                            .unwrap_or_default(),
                    }
                })
                .collect::<Vec<_>>();

            JstackFrequencyRow {
                thread_name,
                thread_id,
                total_count,
                cells,
            }
        })
        .collect::<Vec<_>>();

    rows.sort_by(|left, right| {
        right
            .total_count
            .cmp(&left.total_count)
            .then_with(|| left.thread_name.cmp(&right.thread_name))
            .then_with(|| left.thread_id.cmp(&right.thread_id))
    });

    JstackAnalysisResult {
        snapshots,
        rows,
        skipped_snapshots,
        total_files,
        total_samples,
    }
}

/// 读取一个来源并解析为快照。
fn read_jstack_snapshot(
    target: JstackAnalysisTarget,
    default_encoding: &str,
    loader_config: &LoaderConfig,
) -> Result<JstackSnapshot> {
    let location = resolve_jstack_target_location(&target, loader_config)?;
    let handle = LogFileReader::open(OpenLogRequest {
        location,
        label: target.label.clone(),
        default_encoding: default_encoding.to_string(),
        archive_passwords: target.archive_passwords.clone(),
    })?;
    let samples = parse_jstack_document(handle.document())?;
    Ok(JstackSnapshot {
        source_id: target.source_id,
        label: target.label,
        path: target.path,
        samples,
    })
}

/// 解析 Jstack 输入目标的真实读取位置；待探测压缩包会在后台独立判断是否为单文件日志。
fn resolve_jstack_target_location(
    target: &JstackAnalysisTarget,
    loader_config: &LoaderConfig,
) -> Result<SourceLocation> {
    resolve_analysis_location(
        target.source_id,
        &target.location,
        target.archive_probe_node.as_ref(),
        &target.archive_passwords,
        loader_config,
    )
}

/// 按批次读取日志文档并增量解析 Jstack，避免把完整日志拼成一个大字符串。
fn parse_jstack_document(document: &LogDocument) -> Result<Vec<JstackThreadSample>> {
    let mut parser = JstackSnapshotParser::default();
    let line_count = document.line_count();
    let mut start_line = 0_usize;
    const READ_BATCH_LINES: usize = 4096;

    while start_line < line_count {
        let lines = document.lines(start_line, READ_BATCH_LINES)?;
        if lines.is_empty() {
            break;
        }
        for line in &lines {
            parser.push_line(&line.text);
        }
        start_line += lines.len();
    }

    Ok(parser.finish())
}

impl JstackSnapshotParser {
    /// 追加一行 Jstack 文本并更新当前线程块状态。
    fn push_line(&mut self, line: &str) {
        if let Some((thread_name, thread_id)) = parse_thread_header(line) {
            self.flush_current_thread();
            self.current_thread = Some((thread_name, thread_id));
            self.current_state = JstackThreadState::Other;
            self.current_stack_lines.push(line.to_string());
            return;
        }

        if self.current_thread.is_some() {
            // 当前线程块内的所有原始行都保留下来，详情窗口可以直接展示完整上下文。
            self.current_stack_lines.push(line.to_string());
            if let Some((_, state_text)) = line.split_once("java.lang.Thread.State:") {
                self.current_state = JstackThreadState::from_state_text(state_text);
            }
        }
    }

    /// 完成解析并返回线程样本。
    fn finish(mut self) -> Vec<JstackThreadSample> {
        self.flush_current_thread();
        self.samples
    }

    /// 把当前线程块写入结果，并清空临时状态。
    fn flush_current_thread(&mut self) {
        flush_thread_sample(
            &mut self.samples,
            self.current_thread.take(),
            self.current_state,
            std::mem::take(&mut self.current_stack_lines),
        );
    }
}

/// 把当前线程写入样本列表。
fn flush_thread_sample(
    samples: &mut Vec<JstackThreadSample>,
    thread_identity: Option<(String, String)>,
    state: JstackThreadState,
    stack_lines: Vec<String>,
) {
    if let Some((thread_name, thread_id)) = thread_identity {
        samples.push(JstackThreadSample {
            thread_name,
            thread_id,
            state,
            stack_lines: Arc::from(stack_lines),
        });
    }
}

/// 解析 Jstack 线程头；只接受行首双引号中的线程名，并提取 `#id`、`tid=`、`nid=` 作为身份补充。
fn parse_thread_header(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('"') {
        return None;
    }

    let rest = &trimmed[1..];
    let end = rest.find('"')?;
    let thread_name = &rest[..end];
    if thread_name.is_empty() {
        None
    } else {
        let header_tail = rest[end + 1..].trim_start();
        Some((
            thread_name.to_string(),
            parse_thread_id_summary(header_tail),
        ))
    }
}

/// 拼出 UI 展示用线程身份；没有可用线程 ID 时保持旧版线程名展示。
fn thread_display_label(thread_name: &str, thread_id: &str) -> String {
    if thread_id.is_empty() {
        thread_name.to_string()
    } else {
        format!("{thread_name} · {thread_id}")
    }
}

/// 从 jstack 线程头尾部提取线程 ID 摘要。
///
/// 说明：不同 JVM 输出可能同时包含 `#id`、`tid=` 和 `nid=`，这里全部保留用于区分同名线程；
/// 如果这些字段都缺失，则返回空串并退回到按线程名聚合。
fn parse_thread_id_summary(header_tail: &str) -> String {
    let mut parts = Vec::new();
    if let Some(java_thread_id) = header_tail
        .split_whitespace()
        .find(|token| token.starts_with('#') && token[1..].chars().all(|ch| ch.is_ascii_digit()))
    {
        parts.push(java_thread_id.to_string());
    }
    if let Some(tid) = thread_header_token(header_tail, "tid=") {
        parts.push(tid);
    }
    if let Some(nid) = thread_header_token(header_tail, "nid=") {
        parts.push(nid);
    }

    parts.join(" · ")
}

/// 提取线程头中的 `key=value` 片段，保留 key 便于用户识别 ID 来源。
fn thread_header_token(header_tail: &str, prefix: &str) -> Option<String> {
    header_tail
        .split_whitespace()
        .find(|token| token.starts_with(prefix))
        .map(|token| token.trim_end_matches(',').to_string())
}

/// 根据状态出现次数选择主状态，平票时使用业务优先级。
fn dominant_state(state_counts: &BTreeMap<JstackThreadState, usize>) -> Option<JstackThreadState> {
    state_counts
        .iter()
        .max_by(|(left_state, left_count), (right_state, right_count)| {
            left_count.cmp(right_count).then_with(|| {
                right_state
                    .tie_break_priority()
                    .cmp(&left_state.tie_break_priority())
            })
        })
        .map(|(state, _)| *state)
}

/// 解析线程名过滤关键字；过滤适合短词，因此支持常见行内分隔符。
fn parse_thread_name_filter_patterns(raw: &str) -> Vec<JstackThreadNamePattern> {
    raw.split([',', ';', '|', '\n', '\r', '，', '；'])
        .filter_map(|pattern| {
            let pattern = normalized_filter_pattern(pattern)?;
            if pattern.contains('*') || pattern.contains('?') {
                Some(JstackThreadNamePattern::Wildcard(pattern))
            } else {
                Some(JstackThreadNamePattern::Contains(pattern))
            }
        })
        .collect()
}

/// 解析完整线程段过滤片段；使用空行分隔可以保留单个片段内的堆栈换行。
fn parse_stack_segment_filter_patterns(raw: &str) -> Vec<String> {
    let legacy_delimiter_normalized = raw.replace("||", "\n\n");
    let unescaped = unescape_stack_filter_pattern(&legacy_delimiter_normalized);
    split_stack_segment_filter_blocks(&unescaped)
        .into_iter()
        .filter_map(|pattern| normalized_filter_pattern(&pattern))
        .collect()
}

/// 按空行切分完整线程段过滤配置，忽略连续空行产生的空片段。
///
/// 说明：标准 jstack 在线程块内部也可能包含空行，例如 `Locked ownable
/// synchronizers` 段前的空行；这些续段不应被拆成独立过滤条件，否则会让
/// `- None` 这类通用文本误过滤大量线程。
pub(crate) fn split_stack_segment_filter_blocks(raw: &str) -> Vec<String> {
    let lines = raw.lines().collect::<Vec<_>>();
    let mut blocks = Vec::new();
    let mut current_lines = Vec::new();

    for (line_index, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            if stack_segment_blank_line_should_split(&lines, line_index) {
                push_stack_segment_filter_block(&mut blocks, &mut current_lines);
            } else if !current_lines.is_empty() {
                current_lines.push(String::new());
            }
        } else {
            current_lines.push((*line).to_string());
        }
    }
    push_stack_segment_filter_block(&mut blocks, &mut current_lines);

    blocks
}

/// 判断当前空行是否是真正的多个过滤块分隔符。
fn stack_segment_blank_line_should_split(lines: &[&str], line_index: usize) -> bool {
    next_non_empty_line_after(lines, line_index)
        .is_some_and(|line| !is_jstack_thread_block_continuation_after_blank(line))
}

/// 查找当前行之后的下一个非空行。
fn next_non_empty_line_after<'a>(lines: &'a [&str], line_index: usize) -> Option<&'a str> {
    lines
        .iter()
        .skip(line_index + 1)
        .find(|line| !line.trim().is_empty())
        .copied()
}

/// 识别标准 jstack 在线程块内部空行之后的续段标题或条目。
fn is_jstack_thread_block_continuation_after_blank(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("Locked ownable synchronizers:")
        || trimmed.starts_with("Locked synchronizers:")
        || trimmed.starts_with("JNI global references:")
        || trimmed.starts_with("JNI weak global references:")
        || trimmed.starts_with("- ")
}

/// 将当前收集的线程段写入结果，并清空临时行缓存。
fn push_stack_segment_filter_block(blocks: &mut Vec<String>, current_lines: &mut Vec<String>) {
    if current_lines.is_empty() {
        return;
    }

    let block = current_lines.join("\n");
    if !block.trim().is_empty() {
        blocks.push(block);
    }
    current_lines.clear();
}

/// 归一化过滤片段，空片段返回 `None`。
fn normalized_filter_pattern(pattern: &str) -> Option<String> {
    let pattern = pattern.trim().to_lowercase();
    (!pattern.is_empty()).then_some(pattern)
}

/// 生成线程块过滤专用文本；分析阶段预计算一次，后续矩阵渲染只做包含判断。
fn normalized_stack_search_text(stack_lines: &[String]) -> Arc<str> {
    Arc::<str>::from(stack_lines.join("\n").to_lowercase())
}

/// 把设置页单行输入中的转义字符还原为真实线程段字符。
fn unescape_stack_filter_pattern(pattern: &str) -> String {
    pattern
        .replace("\\r\\n", "\n")
        .replace("\\n", "\n")
        .replace("\\t", "\t")
}

/// 使用常见 glob 语义匹配线程名：`*` 匹配任意长度，`?` 匹配单个字符。
fn wildcard_pattern_matches(pattern: &str, text: &str) -> bool {
    let pattern = pattern.chars().collect::<Vec<_>>();
    let text = text.chars().collect::<Vec<_>>();
    let mut pattern_index = 0_usize;
    let mut text_index = 0_usize;
    let mut star_index = None;
    let mut match_after_star = 0_usize;

    while text_index < text.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == '?' || pattern[pattern_index] == text[text_index])
        {
            pattern_index += 1;
            text_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == '*' {
            star_index = Some(pattern_index);
            pattern_index += 1;
            match_after_star = text_index;
        } else if let Some(star_index) = star_index {
            pattern_index = star_index + 1;
            match_after_star += 1;
            text_index = match_after_star;
        } else {
            return false;
        }
    }

    pattern[pattern_index..]
        .iter()
        .all(|character| *character == '*')
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::loader::archive::ArchiveFormat;
    use crate::loader::{SourceKind, SourceLocation, SourceMetadata};

    use super::*;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    /// 返回标准 Jstack 文本片段。
    fn sample_jstack_text() -> &'static str {
        r#""main" #1 prio=5 os_prio=31 tid=0x0000000149010000 nid=0x1703 runnable [0x000000016f4a7000]
   java.lang.Thread.State: RUNNABLE
        at app.Main.run(Main.java:10)

"worker-1" #2 prio=5 tid=0x0000000149011000 nid=0x1707 waiting on condition
   java.lang.Thread.State: WAITING (parking)
        at jdk.internal.misc.Unsafe.park(Native Method)

"blocked-1" #3 prio=5 tid=0x0000000149012000 nid=0x1708 waiting for monitor entry
   java.lang.Thread.State: BLOCKED (on object monitor)
"#
    }

    /// 验证标准 Jstack 能解析线程名和状态。
    #[test]
    fn parses_standard_jstack_threads() {
        let snapshot =
            parse_jstack_snapshot(SourceId(1), "one.log", "/tmp/one.log", sample_jstack_text());

        assert_eq!(snapshot.samples.len(), 3);
        assert_eq!(snapshot.samples[0].thread_name, "main");
        assert!(snapshot.samples[0].thread_id.contains("#1"));
        assert!(
            snapshot.samples[0]
                .thread_id
                .contains("tid=0x0000000149010000")
        );
        assert_eq!(snapshot.samples[0].state, JstackThreadState::Runnable);
        assert!(snapshot.samples[0].stack_lines[0].contains("\"main\""));
        assert!(
            snapshot.samples[0]
                .stack_lines
                .iter()
                .any(|line| line.contains("app.Main.run"))
        );
        assert_eq!(snapshot.samples[1].thread_name, "worker-1");
        assert_eq!(snapshot.samples[1].state, JstackThreadState::Waiting);
        assert_eq!(snapshot.samples[2].state, JstackThreadState::Blocked);
    }

    /// 验证缺失或未知状态会归为 OTHER。
    #[test]
    fn parses_missing_and_unknown_state_as_other() {
        let snapshot = parse_jstack_snapshot(
            SourceId(1),
            "bad.log",
            "/tmp/bad.log",
            r#""no-state" #1 tid=0x1
        at app.Main.run(Main.java:10)
"unknown-state" #2 tid=0x2
   java.lang.Thread.State: VIRTUAL_WAITING
"#,
        );

        assert_eq!(snapshot.samples.len(), 2);
        assert!(
            snapshot
                .samples
                .iter()
                .all(|sample| sample.state == JstackThreadState::Other)
        );
    }

    /// 验证空文件和非 Jstack 文本不会产生线程样本。
    #[test]
    fn ignores_empty_and_non_jstack_text() {
        let empty = parse_jstack_snapshot(SourceId(1), "empty.log", "/tmp/empty.log", "");
        let plain = parse_jstack_snapshot(SourceId(2), "plain.log", "/tmp/plain.log", "INFO hello");

        assert!(empty.samples.is_empty());
        assert!(plain.samples.is_empty());
    }

    /// 验证同名同 ID 线程会累加频率，同名不同 ID 线程会拆成独立行。
    #[test]
    fn aggregates_duplicate_thread_names() {
        let snapshot = parse_jstack_snapshot(
            SourceId(1),
            "dup.log",
            "/tmp/dup.log",
            r#""pool-1" #1
   java.lang.Thread.State: RUNNABLE
"pool-1" #1
   java.lang.Thread.State: BLOCKED (on object monitor)
"pool-1" #2
   java.lang.Thread.State: RUNNABLE
"pool-2" #3
   java.lang.Thread.State: TIMED_WAITING (sleeping)
"#,
        );
        let result = build_analysis_result(vec![snapshot], Vec::new(), 1);

        let pool_1 = result
            .rows
            .iter()
            .find(|row| row.thread_name == "pool-1" && row.thread_id.contains("#1"))
            .expect("应存在同名线程聚合行");
        assert_eq!(pool_1.total_count, 2);
        assert_eq!(pool_1.cells[0].count, 2);
        assert_eq!(pool_1.cells[0].state, Some(JstackThreadState::Blocked));
        assert_eq!(pool_1.cells[0].stack_occurrences.len(), 2);
        assert_eq!(pool_1.cells[0].stack_occurrences[1].occurrence_index, 2);
        let pool_1_other_id = result
            .rows
            .iter()
            .find(|row| row.thread_name == "pool-1" && row.thread_id.contains("#2"))
            .expect("同名不同 ID 线程应拆成独立行");
        assert_eq!(pool_1_other_id.total_count, 1);
    }

    /// 验证矩阵按快照输入顺序生成列，并按总频率排序行。
    #[test]
    fn builds_frequency_matrix_in_snapshot_order() {
        let first = parse_jstack_snapshot(
            SourceId(1),
            "001.log",
            "/tmp/001.log",
            r#""busy" #1
   java.lang.Thread.State: RUNNABLE
"idle" #2
   java.lang.Thread.State: WAITING (parking)
"#,
        );
        let second = parse_jstack_snapshot(
            SourceId(2),
            "002.log",
            "/tmp/002.log",
            r#""busy" #1
   java.lang.Thread.State: RUNNABLE
"busy" #1
   java.lang.Thread.State: RUNNABLE
"#,
        );
        let result = build_analysis_result(vec![first, second], Vec::new(), 2);

        assert_eq!(result.snapshots[0].label, "001.log");
        assert_eq!(result.snapshots[1].label, "002.log");
        assert_eq!(result.rows[0].thread_name, "busy");
        assert_eq!(result.rows[0].total_count, 3);
        assert_eq!(result.rows[0].cells[0].count, 1);
        assert_eq!(result.rows[0].cells[1].count, 2);
        assert_eq!(
            result.rows[0].cells[0].stack_occurrences[0].snapshot_label,
            "001.log"
        );
        assert_eq!(
            result.rows[0].cells[1].stack_occurrences[1].occurrence_index,
            2
        );
        assert_eq!(result.rows[1].thread_name, "idle");
        assert_eq!(result.rows[1].cells[1].count, 0);
        assert_eq!(result.rows[1].cells[1].state, None);
        assert!(result.rows[1].cells[1].stack_occurrences.is_empty());
    }

    /// 验证线程名过滤使用大小写不敏感的模糊匹配。
    #[test]
    fn thread_filter_matches_thread_name_patterns() {
        let snapshot = parse_jstack_snapshot(
            SourceId(1),
            "filter.log",
            "/tmp/filter.log",
            r#""Attach Listener" #1
   java.lang.Thread.State: RUNNABLE
"business-worker" #2
   java.lang.Thread.State: RUNNABLE
"#,
        );
        let result = build_analysis_result(vec![snapshot], Vec::new(), 1);
        let filter = JstackThreadFilter::from_raw("listener", "");

        assert!(
            filter.matches_row(
                result
                    .rows
                    .iter()
                    .find(|row| row.thread_name == "Attach Listener")
                    .expect("应存在 Attach Listener 行")
            )
        );
        assert!(
            !filter.matches_row(
                result
                    .rows
                    .iter()
                    .find(|row| row.thread_name == "business-worker")
                    .expect("应存在业务线程行")
            )
        );
    }

    /// 验证线程名过滤支持 `*` 和 `?` 通配符，并按完整线程名匹配。
    #[test]
    fn thread_filter_matches_thread_name_wildcards() {
        let snapshot = parse_jstack_snapshot(
            SourceId(1),
            "wildcard.log",
            "/tmp/wildcard.log",
            r#""dasc-jetty-qtp-892335322-126905" #1
   java.lang.Thread.State: RUNNABLE
"dasc-jetty-qtp-892335322-17" #2
   java.lang.Thread.State: RUNNABLE
"business-worker" #3
   java.lang.Thread.State: RUNNABLE
"#,
        );
        let result = build_analysis_result(vec![snapshot], Vec::new(), 1);
        let filter = JstackThreadFilter::from_raw("dasc-jetty-*-??????", "");

        assert!(
            filter.matches_row(
                result
                    .rows
                    .iter()
                    .find(|row| row.thread_name == "dasc-jetty-qtp-892335322-126905")
                    .expect("应存在匹配通配符的线程行")
            )
        );
        assert!(
            !filter.matches_row(
                result
                    .rows
                    .iter()
                    .find(|row| row.thread_name == "dasc-jetty-qtp-892335322-17")
                    .expect("应存在位数不匹配的线程行")
            )
        );
        assert!(
            !filter.matches_row(
                result
                    .rows
                    .iter()
                    .find(|row| row.thread_name == "business-worker")
                    .expect("应存在业务线程行")
            )
        );
    }

    /// 验证完整线程段过滤使用空行分隔，并兼容旧版 `||` 配置。
    #[test]
    fn stack_segment_filter_patterns_split_on_blank_lines() {
        let patterns = parse_stack_segment_filter_patterns(
            "SocketInputStream.socketRead\n    at java.net.SocketInputStream.read\n\n\nUnsafe.park\\nLockSupport.park",
        );

        assert_eq!(
            patterns,
            vec![
                "socketinputstream.socketread\n    at java.net.socketinputstream.read",
                "unsafe.park\nlocksupport.park"
            ]
        );

        let legacy_patterns =
            parse_stack_segment_filter_patterns("Unsafe.park||SocketInputStream\\nread");
        assert_eq!(
            legacy_patterns,
            vec!["unsafe.park", "socketinputstream\nread"]
        );
    }

    /// 验证标准 jstack 线程块内部的空行不会把同步器段拆成独立过滤条件。
    #[test]
    fn stack_segment_filter_keeps_thread_internal_blank_lines() {
        let patterns = parse_stack_segment_filter_patterns(
            "java.lang.Thread.State: RUNNABLE\n    at java.net.SocketInputStream.read(SocketInputStream.java:171)\n\n    Locked ownable synchronizers:\n    - None\n\njava.lang.Thread.State: WAITING\n    at sun.misc.Unsafe.park(Native Method)",
        );

        assert_eq!(patterns.len(), 2);
        assert!(patterns[0].contains("socketinputstream.read"));
        assert!(patterns[0].contains("locked ownable synchronizers"));
        assert!(patterns[0].contains("- none"));
        assert!(patterns[1].contains("java.lang.thread.state: waiting"));
        assert!(patterns[1].contains("unsafe.park"));
    }

    /// 验证完整线程段过滤能匹配转义换行后的堆栈片段。
    #[test]
    fn thread_filter_matches_stack_segment_patterns() {
        let snapshot = parse_jstack_snapshot(
            SourceId(1),
            "stack.log",
            "/tmp/stack.log",
            r#""socket-reader" #1
   java.lang.Thread.State: RUNNABLE
        at java.net.SocketInputStream.socketRead0(Native Method)
        at java.net.SocketInputStream.socketRead(SocketInputStream.java:116)
"business-worker" #2
   java.lang.Thread.State: RUNNABLE
        at app.Business.run(Business.java:10)
"#,
        );
        let result = build_analysis_result(vec![snapshot], Vec::new(), 1);
        let filter = JstackThreadFilter::from_raw(
            "",
            "java.net.SocketInputStream.socketRead0(Native Method)\\n        at java.net.SocketInputStream.socketRead",
        );
        let socket_occurrence = result
            .rows
            .iter()
            .find(|row| row.thread_name == "socket-reader")
            .expect("应存在 socket-reader 行")
            .cells[0]
            .stack_occurrences
            .first()
            .expect("应存在 socket-reader 堆栈记录");

        assert!(
            filter.matches_row(
                result
                    .rows
                    .iter()
                    .find(|row| row.thread_name == "socket-reader")
                    .expect("应存在 socket-reader 行")
            )
        );
        assert!(
            !filter.matches_row(
                result
                    .rows
                    .iter()
                    .find(|row| row.thread_name == "business-worker")
                    .expect("应存在业务线程行")
            )
        );
        assert!(
            socket_occurrence
                .normalized_stack_text
                .contains("socketinputstream.socketread0")
        );
    }

    /// 验证通过 LogFileReader 的读取集成路径，并记录失败来源。
    #[test]
    fn analyzes_targets_with_reader_and_skips_failures() {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间应晚于 UNIX_EPOCH")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "argus-jstack-analysis-{}-{timestamp}.log",
            std::process::id()
        ));
        fs::write(&path, sample_jstack_text()).expect("应能写入 Jstack 测试日志");
        let missing_path = path.with_extension("missing");

        let result = analyze_jstack_targets(
            vec![
                JstackAnalysisTarget {
                    source_id: SourceId(1),
                    location: SourceLocation::LocalPath(path.clone()),
                    archive_probe_node: None,
                    label: "ok.log".to_string(),
                    path: path.display().to_string(),
                    archive_passwords: ArchivePasswordStore::default(),
                },
                JstackAnalysisTarget {
                    source_id: SourceId(2),
                    location: SourceLocation::LocalPath(missing_path),
                    archive_probe_node: None,
                    label: "missing.log".to_string(),
                    path: "missing.log".to_string(),
                    archive_passwords: ArchivePasswordStore::default(),
                },
            ],
            "UTF-8".to_string(),
            LoaderConfig::default(),
        );

        assert_eq!(result.total_files, 2);
        assert_eq!(result.snapshot_count(), 1);
        assert_eq!(result.skipped_count(), 1);
        assert_eq!(result.thread_count(), 3);

        let _ = fs::remove_file(path);
    }

    /// 验证本地目录目标会递归展开为多个 Jstack 快照，非候选文件不会进入统计。
    #[test]
    fn analyzes_local_directory_recursively() {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间应晚于 UNIX_EPOCH")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "argus-jstack-analysis-dir-{}-{timestamp}",
            std::process::id()
        ));
        let nested_dir = dir.join("nested");
        fs::create_dir_all(&nested_dir).expect("应能创建 Jstack 目录测试路径");
        fs::write(dir.join("thread-a.log"), sample_jstack_text()).expect("应能写入 Jstack 日志");
        fs::write(nested_dir.join("thread-b.txt"), sample_jstack_text())
            .expect("应能写入嵌套 Jstack 日志");
        fs::write(dir.join("ignore.dat"), sample_jstack_text()).expect("应能写入非候选文件");

        let result = analyze_jstack_targets(
            vec![JstackAnalysisTarget {
                source_id: SourceId(9),
                location: SourceLocation::LocalPath(dir.clone()),
                archive_probe_node: None,
                label: "thread-dir".to_string(),
                path: dir.display().to_string(),
                archive_passwords: ArchivePasswordStore::default(),
            }],
            "UTF-8".to_string(),
            LoaderConfig::default(),
        );

        assert_eq!(result.total_files, 2);
        assert_eq!(result.snapshot_count(), 2);
        assert_eq!(result.skipped_count(), 0);
        assert_eq!(result.thread_count(), 3);

        let _ = fs::remove_dir_all(dir);
    }

    /// 验证开启跟随符号链接时，目录回环不会导致 Jstack 递归扫描卡死。
    #[cfg(unix)]
    #[test]
    fn jstack_directory_recursion_skips_symlink_cycles() {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间应晚于 UNIX_EPOCH")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "argus-jstack-analysis-symlink-{}-{timestamp}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("应能创建 Jstack 符号链接测试路径");
        fs::write(dir.join("thread-a.log"), sample_jstack_text()).expect("应能写入 Jstack 日志");
        std::os::unix::fs::symlink(&dir, dir.join("loop")).expect("应能创建目录符号链接回环");
        let config = LoaderConfig {
            follow_symlinks: true,
            ..LoaderConfig::default()
        };

        let result = analyze_jstack_targets(
            vec![JstackAnalysisTarget {
                source_id: SourceId(10),
                location: SourceLocation::LocalPath(dir.clone()),
                archive_probe_node: None,
                label: "thread-dir".to_string(),
                path: dir.display().to_string(),
                archive_passwords: ArchivePasswordStore::default(),
            }],
            "UTF-8".to_string(),
            config,
        );

        assert_eq!(result.total_files, 1);
        assert_eq!(result.snapshot_count(), 1);
        assert_eq!(result.skipped_count(), 0);

        let _ = fs::remove_dir_all(dir);
    }

    /// 验证 Jstack 分析能独立探测待识别的单文件压缩包，不依赖来源树先完成节点替换。
    #[test]
    fn analyzes_pending_single_file_archive_without_registry_replacement() {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间应晚于 UNIX_EPOCH")
            .as_nanos();
        let zip_path = std::env::temp_dir().join(format!(
            "argus-jstack-analysis-{}-{timestamp}.zip",
            std::process::id()
        ));
        let file = fs::File::create(&zip_path).expect("应能创建 Jstack ZIP 测试文件");
        let mut writer = ZipWriter::new(file);
        writer
            .start_file("thread.log", SimpleFileOptions::default())
            .expect("应能写入 ZIP 条目");
        writer
            .write_all(sample_jstack_text().as_bytes())
            .expect("应能写入 Jstack 文本");
        writer.finish().expect("应能完成 ZIP 写入");

        let source_id = SourceId(7);
        let archive_node = SourceTreeNode {
            id: source_id,
            parent_id: None,
            depth: 0,
            label: "thread.zip".to_string(),
            kind: SourceKind::Archive(ArchiveFormat::Zip),
            location: SourceLocation::LocalPath(zip_path.clone()),
            metadata: SourceMetadata {
                size: fs::metadata(&zip_path).ok().map(|metadata| metadata.len()),
                children_loaded: false,
                is_loading: true,
                message: None,
            },
            selected: false,
            expanded: false,
        };

        let result = analyze_jstack_targets(
            vec![JstackAnalysisTarget {
                source_id,
                location: SourceLocation::LocalPath(PathBuf::from(&zip_path)),
                archive_probe_node: Some(archive_node),
                label: "thread.zip".to_string(),
                path: zip_path.display().to_string(),
                archive_passwords: ArchivePasswordStore::default(),
            }],
            "UTF-8".to_string(),
            LoaderConfig::default(),
        );

        assert_eq!(result.total_files, 1);
        assert_eq!(result.snapshot_count(), 1);
        assert_eq!(result.skipped_count(), 0);
        assert_eq!(result.thread_count(), 3);

        let _ = fs::remove_file(zip_path);
    }
}
