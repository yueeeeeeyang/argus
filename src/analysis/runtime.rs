//! 文件职责：实现 Runtime 请求日志解析、聚合统计和读取入口。
//! 创建日期：2026-06-25
//! 修改日期：2026-06-25
//! 作者：Argus 开发团队
//! 主要功能：解析运行期请求耗时日志，按请求地址合并统计并保留请求 SQL 明细。

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use anyhow::{Context as _, Result, anyhow, bail};
use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};

use crate::config::LoaderConfig;
use crate::loader::archive::{ArchiveFormat, ArchivePasswordKey, ArchivePasswordStore};
use crate::loader::{
    LogSourceLoader, SourceArchiveProbeRequest, SourceId, SourceLocation, SourceTreeNode,
};
use crate::reader::encoding_detector::{decode_log_bytes, decode_log_bytes_with_known_encoding};
use crate::reader::stream_backend::ArchiveStreamBackend;
use crate::utils::path::normalize_archive_entry_path;
use zip::ZipArchive;

/// Runtime 请求日志慢 SQL 判断比例；SQL 累积耗时超过请求总耗时 90% 即认为该请求慢。
const SLOW_SQL_REQUEST_PERCENT: u64 = 90;
/// Runtime 日志并行解析任务上限，避免大量文件时把磁盘和压缩包读取线程打满。
const MAX_RUNTIME_PARSE_WORKERS: usize = 8;
/// ZIP 条目数量达到该阈值后才拆分为多 worker，避免少量条目重复打开 ZIP。
const MIN_PARALLEL_ZIP_TARGETS: usize = 64;

/// 分析任务输入目标类型。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeAnalysisTargetKind {
    /// 单个日志文件，可能来自本地或压缩包条目。
    File,
    /// 本地目录；后台会递归收集其中的 `.log` 文件。
    Directory,
}

/// Runtime 分析任务输入目标。
#[derive(Clone, Debug)]
pub struct RuntimeAnalysisTarget {
    /// 来源树节点 ID；目录递归生成的子文件沿用目录 ID 作为任务上下文。
    pub source_id: SourceId,
    /// 来源位置，目录仅支持本地路径。
    pub location: SourceLocation,
    /// 待探测单文件压缩包节点快照；存在时后台会先独立探测真实日志条目。
    pub archive_probe_node: Option<SourceTreeNode>,
    /// UI 展示名称。
    pub label: String,
    /// 路径展示文本。
    pub path: String,
    /// 当前目标是文件还是目录。
    pub kind: RuntimeAnalysisTargetKind,
    /// 当前会话中已输入的压缩包密码快照。
    pub archive_passwords: ArchivePasswordStore,
}

/// 单条 SQL 明细记录。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSqlRecord {
    /// SQL 执行总耗时，单位毫秒。
    pub execute_ms: u64,
    /// 获取数据库连接耗时，单位毫秒。
    pub acquire_connection_ms: u64,
    /// 事务提交耗时，单位毫秒。
    pub commit_ms: u64,
    /// 释放连接耗时，单位毫秒。
    pub release_connection_ms: u64,
    /// 解析结果集耗时，单位毫秒。
    pub parse_result_ms: u64,
    /// SQL 原文，可能包含换行。
    pub sql_text: String,
    /// SQL 结构归一化文本，用于频率分析时避免重复解析 SQL 原文。
    pub normalized_sql: String,
}

/// SQL 频率分析行。
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeSqlFrequencyAnalysisRow {
    /// 归一化后的 SQL 结构文本。
    pub normalized_sql: String,
    /// 当前结构下所有 SQL 的执行总耗时。
    pub total_execute_ms: u64,
    /// 当前结构命中的 SQL 执行次数。
    pub execute_count: usize,
}

impl RuntimeSqlFrequencyAnalysisRow {
    /// 返回当前 SQL 结构的平均执行耗时。
    pub fn average_execute_ms(&self) -> f64 {
        if self.execute_count == 0 {
            0.0
        } else {
            self.total_execute_ms as f64 / self.execute_count as f64
        }
    }
}

/// 慢 SQL 归一化聚合行。
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeSlowSqlSummaryRow {
    /// 归一化后的 SQL 结构文本。
    pub normalized_sql: String,
    /// 当前结构下所有 SQL 的执行总耗时。
    pub total_execute_ms: u64,
    /// 当前结构命中的 SQL 执行次数。
    pub execute_count: usize,
}

impl RuntimeSlowSqlSummaryRow {
    /// 返回当前 SQL 结构的平均执行耗时。
    pub fn average_execute_ms(&self) -> f64 {
        if self.execute_count == 0 {
            0.0
        } else {
            self.total_execute_ms as f64 / self.execute_count as f64
        }
    }
}

/// SQL 频率详情行，表示某个 SQL 结构的一次具体执行。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSqlFrequencyDetailRow {
    /// 请求记录在结果集中的稳定索引。
    pub request_index: usize,
    /// SQL 记录在当前请求中的稳定索引。
    pub sql_index: usize,
    /// SQL 单次执行耗时。
    pub execute_ms: u64,
}

/// 单个请求日志文件解析后的记录。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeRequestRecord {
    /// 记录在 `RuntimeAnalysisResult.requests` 中的稳定索引。
    pub index: usize,
    /// 来源树节点 ID。
    pub source_id: SourceId,
    /// 文件展示名称。
    pub label: String,
    /// 文件路径或虚拟路径。
    pub path: String,
    /// 用户名；文件名中为空时保存为空字符串。
    pub username: String,
    /// 用户名小写副本，用于用户名过滤时避免重复分配字符串。
    pub username_lowercase: String,
    /// 请求地址，已把 `_` 转为 `/`。
    pub request_path: String,
    /// 请求总耗时，单位毫秒。
    pub request_duration_ms: u64,
    /// 请求时间戳，单位毫秒。
    pub request_timestamp_ms: i64,
    /// 本地时区格式化后的请求时间。
    pub request_time_label: String,
    /// socket 耗时，单位毫秒。
    pub socket_duration_ms: u64,
    /// 安全校验耗时，单位毫秒。
    pub security_check_ms: u64,
    /// SQL 明细列表。
    pub sql_records: Vec<RuntimeSqlRecord>,
    /// SQL 执行总耗时累加值，单位毫秒。
    pub sql_total_execute_ms: u64,
    /// 是否为慢 SQL 请求日志。
    pub is_slow_sql_request: bool,
}

/// 按请求地址合并后的总览行。
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeRequestSummary {
    /// 请求地址。
    pub request_path: String,
    /// 请求日志数量。
    pub request_count: usize,
    /// 平均请求耗时，单位毫秒。
    pub average_duration_ms: f64,
    /// 慢 SQL 请求日志数量。
    pub slow_request_count: usize,
    /// 慢 SQL 请求比例，范围 0.0..=1.0。
    pub slow_sql_ratio: f64,
    /// 当前请求地址下的请求记录索引列表。
    pub request_indices: Vec<usize>,
}

/// 被跳过或读取失败的 Runtime 日志。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSkippedFile {
    /// 来源树节点 ID。
    pub source_id: SourceId,
    /// 文件展示名称。
    pub label: String,
    /// 跳过原因。
    pub reason: String,
}

/// Runtime 分析结果，供 UI 三层表格直接读取。
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeAnalysisResult {
    /// 所有成功解析的请求记录。
    pub requests: Vec<RuntimeRequestRecord>,
    /// 按请求地址合并后的总览行。
    pub summaries: Vec<RuntimeRequestSummary>,
    /// 跳过或读取失败的文件。
    pub skipped_files: Vec<RuntimeSkippedFile>,
    /// 本次实际尝试解析的 `.log` 文件数量。
    pub total_files: usize,
    /// SQL 明细总数。
    pub total_sql_records: usize,
}

/// Runtime 分析过滤输入快照，保存用户在过滤栏中输入的原始文本。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RuntimeAnalysisFilterSnapshot {
    /// 任意关键字过滤输入原文。
    pub keyword: String,
    /// 用户名过滤输入原文。
    pub username: String,
    /// 开始时间过滤输入原文。
    pub start_time: String,
    /// 结束时间过滤输入原文。
    pub end_time: String,
}

/// Runtime 分析过滤条件，解析一次后供后台缓存和 UI 回退路径复用。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RuntimeAnalysisFilterCriteria {
    /// 关键字小写文本，空字符串表示不启用。
    pub keyword: String,
    /// 用户名过滤关键字列表，空列表表示不启用。
    pub usernames: Vec<String>,
    /// 开始时间戳，单位毫秒。
    pub start_timestamp_ms: Option<i64>,
    /// 结束时间戳，单位毫秒。
    pub end_timestamp_ms: Option<i64>,
}

impl RuntimeAnalysisFilterCriteria {
    /// 返回是否配置了任意有效过滤条件。
    pub fn is_active(&self) -> bool {
        !self.keyword.is_empty()
            || !self.usernames.is_empty()
            || self.start_timestamp_ms.is_some()
            || self.end_timestamp_ms.is_some()
    }

    /// 判断关键字是否为空或命中已经小写化的文本。
    pub fn keyword_matches_lowercase(&self, lowercase_text: &str) -> bool {
        self.keyword.is_empty() || lowercase_text.contains(&self.keyword)
    }

    /// 判断关键字是否为空或命中普通文本；只用于少量汇总字段的回退匹配。
    pub fn keyword_matches_text(&self, text: &str) -> bool {
        self.keyword.is_empty() || text.to_lowercase().contains(&self.keyword)
    }
}

/// Runtime 过滤后的统一行缓存，供统计、SQL 频率和慢 SQL 三种结果类型共享。
#[derive(Clone, Debug)]
pub struct RuntimeAnalysisFilterRows {
    /// 生成缓存时使用的过滤输入快照。
    pub filter: RuntimeAnalysisFilterSnapshot,
    /// 当前过滤条件下的统计总览行。
    pub summaries: Arc<Vec<RuntimeRequestSummary>>,
    /// 当前过滤条件下每个请求可见的 SQL 索引列表。
    pub sql_indices_by_request: Arc<HashMap<usize, Vec<usize>>>,
}

impl RuntimeAnalysisResult {
    /// 返回成功解析的请求日志数量。
    pub fn request_count(&self) -> usize {
        self.requests.len()
    }

    /// 返回合并后的请求地址数量。
    pub fn summary_count(&self) -> usize {
        self.summaries.len()
    }

    /// 返回跳过文件数量。
    pub fn skipped_count(&self) -> usize {
        self.skipped_files.len()
    }
}

/// 从多个来源读取并分析 Runtime 请求日志。
///
/// 参数说明：
/// - `targets`：按来源树顺序排列的分析目标。
/// - `default_encoding`：日志读取兜底编码。
/// - `loader_config`：日志加载配置，目录递归会尊重符号链接策略。
///
/// 返回值：可直接供 Runtime 分析页渲染的聚合结果。
pub fn analyze_runtime_targets(
    targets: Vec<RuntimeAnalysisTarget>,
    default_encoding: String,
    loader_config: LoaderConfig,
) -> RuntimeAnalysisResult {
    let mut file_targets = Vec::new();
    let mut skipped_files = Vec::new();

    for target in targets {
        match expand_runtime_target(target, &loader_config) {
            Ok(mut expanded) => file_targets.append(&mut expanded),
            Err((source_id, label, reason)) => skipped_files.push(RuntimeSkippedFile {
                source_id,
                label,
                reason,
            }),
        }
    }

    let total_files = file_targets.len();
    let parsed_files =
        read_runtime_requests_parallel(file_targets, &default_encoding, &loader_config);
    let mut requests = Vec::new();
    for parsed_file in parsed_files {
        match parsed_file {
            Ok(mut request) => {
                request.index = requests.len();
                requests.push(request);
            }
            Err(skipped_file) => skipped_files.push(skipped_file),
        }
    }

    build_runtime_analysis_result(requests, skipped_files, total_files)
}

/// 解析单个 Runtime 日志文件文本。
///
/// 参数说明：
/// - `source_id`：来源树节点 ID。
/// - `label`：文件展示名称，必须符合 Runtime 文件命名约定。
/// - `path`：文件路径或虚拟路径。
/// - `text`：文件内容。
///
/// 返回值：单个请求日志记录，索引由上层聚合时写入。
pub fn parse_runtime_request_text(
    source_id: SourceId,
    label: impl Into<String>,
    path: impl Into<String>,
    text: &str,
) -> Result<RuntimeRequestRecord> {
    let label = label.into();
    let path = path.into();
    let metadata = parse_runtime_file_name(&label)?;
    let sql_records = parse_runtime_sql_records(text);
    Ok(build_request_record(
        0,
        source_id,
        label,
        path,
        metadata,
        sql_records,
    ))
}

/// 由请求记录构建总览聚合结果。
pub fn build_runtime_analysis_result(
    requests: Vec<RuntimeRequestRecord>,
    skipped_files: Vec<RuntimeSkippedFile>,
    total_files: usize,
) -> RuntimeAnalysisResult {
    let mut grouped = BTreeMap::<String, Vec<usize>>::new();
    let total_sql_records = requests
        .iter()
        .map(|request| request.sql_records.len())
        .sum::<usize>();

    for request in &requests {
        grouped
            .entry(request.request_path.clone())
            .or_default()
            .push(request.index);
    }

    let mut summaries = grouped
        .into_iter()
        .map(|(request_path, request_indices)| {
            let request_count = request_indices.len();
            let total_duration = request_indices
                .iter()
                .filter_map(|index| requests.get(*index))
                .map(|request| request.request_duration_ms)
                .sum::<u64>();
            let slow_request_count = request_indices
                .iter()
                .filter_map(|index| requests.get(*index))
                .filter(|request| request.is_slow_sql_request)
                .count();
            let average_duration_ms = if request_count == 0 {
                0.0
            } else {
                total_duration as f64 / request_count as f64
            };
            let slow_sql_ratio = if request_count == 0 {
                0.0
            } else {
                slow_request_count as f64 / request_count as f64
            };

            RuntimeRequestSummary {
                request_path,
                request_count,
                average_duration_ms,
                slow_request_count,
                slow_sql_ratio,
                request_indices,
            }
        })
        .collect::<Vec<_>>();

    summaries.sort_by(|left, right| {
        right
            .request_count
            .cmp(&left.request_count)
            .then_with(|| left.request_path.cmp(&right.request_path))
    });

    RuntimeAnalysisResult {
        requests,
        summaries,
        skipped_files,
        total_files,
        total_sql_records,
    }
}

/// 构建无过滤条件下的 SQL 频率分析结果。
pub fn build_runtime_sql_frequency_rows(
    requests: &[RuntimeRequestRecord],
) -> Vec<RuntimeSqlFrequencyAnalysisRow> {
    let mut grouped = BTreeMap::<String, RuntimeSqlFrequencyAnalysisRow>::new();
    for request in requests {
        for sql in &request.sql_records {
            let row = grouped
                .entry(sql.normalized_sql.clone())
                .or_insert_with(|| RuntimeSqlFrequencyAnalysisRow {
                    normalized_sql: sql.normalized_sql.clone(),
                    total_execute_ms: 0,
                    execute_count: 0,
                });
            row.total_execute_ms = row.total_execute_ms.saturating_add(sql.execute_ms);
            row.execute_count = row.execute_count.saturating_add(1);
        }
    }

    let mut rows = grouped.into_values().collect::<Vec<_>>();
    sort_runtime_sql_frequency_rows(&mut rows);
    rows
}

/// 构建无过滤条件下的慢 SQL 分析结果。
pub fn build_runtime_slow_sql_rows(
    requests: &[RuntimeRequestRecord],
) -> Vec<RuntimeSlowSqlSummaryRow> {
    let mut grouped = BTreeMap::<String, RuntimeSlowSqlSummaryRow>::new();
    for request in requests {
        for sql in &request.sql_records {
            let row = grouped
                .entry(sql.normalized_sql.clone())
                .or_insert_with(|| RuntimeSlowSqlSummaryRow {
                    normalized_sql: sql.normalized_sql.clone(),
                    total_execute_ms: 0,
                    execute_count: 0,
                });
            row.total_execute_ms = row.total_execute_ms.saturating_add(sql.execute_ms);
            row.execute_count = row.execute_count.saturating_add(1);
        }
    }
    let mut rows = grouped.into_values().collect::<Vec<_>>();
    sort_runtime_slow_sql_summary_rows(&mut rows);
    rows
}

/// 按过滤条件构建 SQL 频率分析结果；用于进入 SQL 频率页时后台懒计算。
pub fn build_runtime_sql_frequency_rows_for_filter(
    result: &RuntimeAnalysisResult,
    filter: &RuntimeAnalysisFilterSnapshot,
) -> Vec<RuntimeSqlFrequencyAnalysisRow> {
    let criteria = parse_runtime_analysis_filter_criteria(filter);
    if !criteria.is_active() {
        return build_runtime_sql_frequency_rows(&result.requests);
    }

    let mut grouped = BTreeMap::<String, RuntimeSqlFrequencyAnalysisRow>::new();
    for request in &result.requests {
        if !runtime_request_matches_cross_filters(request, &criteria) {
            continue;
        }

        for sql in &request.sql_records {
            if !runtime_sql_matches_keyword(request, sql, &criteria) {
                continue;
            }

            let row = grouped
                .entry(sql.normalized_sql.clone())
                .or_insert_with(|| RuntimeSqlFrequencyAnalysisRow {
                    normalized_sql: sql.normalized_sql.clone(),
                    total_execute_ms: 0,
                    execute_count: 0,
                });
            row.total_execute_ms = row.total_execute_ms.saturating_add(sql.execute_ms);
            row.execute_count = row.execute_count.saturating_add(1);
        }
    }

    let mut rows = grouped.into_values().collect::<Vec<_>>();
    sort_runtime_sql_frequency_rows(&mut rows);
    rows
}

/// 按过滤条件构建慢 SQL 分析结果；用于进入慢 SQL 页时后台懒计算。
pub fn build_runtime_slow_sql_rows_for_filter(
    result: &RuntimeAnalysisResult,
    filter: &RuntimeAnalysisFilterSnapshot,
) -> Vec<RuntimeSlowSqlSummaryRow> {
    let criteria = parse_runtime_analysis_filter_criteria(filter);
    if !criteria.is_active() {
        return build_runtime_slow_sql_rows(&result.requests);
    }

    let mut grouped = BTreeMap::<String, RuntimeSlowSqlSummaryRow>::new();
    for request in &result.requests {
        if !runtime_request_matches_cross_filters(request, &criteria) {
            continue;
        }

        for sql in &request.sql_records {
            if !runtime_sql_matches_keyword(request, sql, &criteria) {
                continue;
            }

            let row = grouped
                .entry(sql.normalized_sql.clone())
                .or_insert_with(|| RuntimeSlowSqlSummaryRow {
                    normalized_sql: sql.normalized_sql.clone(),
                    total_execute_ms: 0,
                    execute_count: 0,
                });
            row.total_execute_ms = row.total_execute_ms.saturating_add(sql.execute_ms);
            row.execute_count = row.execute_count.saturating_add(1);
        }
    }
    let mut rows = grouped.into_values().collect::<Vec<_>>();
    sort_runtime_slow_sql_summary_rows(&mut rows);
    rows
}

/// 根据原始过滤输入构造可执行的 Runtime 过滤条件。
pub fn parse_runtime_analysis_filter_criteria(
    filter: &RuntimeAnalysisFilterSnapshot,
) -> RuntimeAnalysisFilterCriteria {
    RuntimeAnalysisFilterCriteria {
        keyword: filter.keyword.trim().to_lowercase(),
        usernames: parse_runtime_username_filters(&filter.username),
        start_timestamp_ms: parse_runtime_filter_time_value(&filter.start_time, false),
        end_timestamp_ms: parse_runtime_filter_time_value(&filter.end_time, true),
    }
}

/// 解析用户名过滤输入，支持英文逗号和中文逗号分隔多个模糊匹配关键字。
pub fn parse_runtime_username_filters(raw: &str) -> Vec<String> {
    raw.split([',', '，'])
        .map(|part| part.trim().to_lowercase())
        .filter(|part| !part.is_empty())
        .collect()
}

/// 解析 Runtime 时间过滤输入；支持毫秒时间戳和常见本地时间格式。
pub fn parse_runtime_filter_time_value(raw: &str, is_end: bool) -> Option<i64> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(timestamp_ms) = value.parse::<i64>() {
        return Some(timestamp_ms);
    }

    for format in [
        "%Y-%m-%d %H:%M:%S%.3f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(datetime) = NaiveDateTime::parse_from_str(value, format)
            && let Some(local_datetime) = Local.from_local_datetime(&datetime).single()
        {
            return Some(local_datetime.timestamp_millis());
        }
    }

    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        let datetime = if is_end {
            date.and_hms_milli_opt(23, 59, 59, 999)
        } else {
            date.and_hms_milli_opt(0, 0, 0, 0)
        }?;
        return Local
            .from_local_datetime(&datetime)
            .single()
            .map(|datetime| datetime.timestamp_millis());
    }

    None
}

/// 在后台构建 Runtime 三类结果共享的过滤行缓存。
pub fn build_runtime_analysis_filter_rows(
    result: &RuntimeAnalysisResult,
    filter: RuntimeAnalysisFilterSnapshot,
) -> RuntimeAnalysisFilterRows {
    let criteria = parse_runtime_analysis_filter_criteria(&filter);
    let mut summaries = Vec::new();
    let mut sql_indices_by_request = HashMap::<usize, Vec<usize>>::new();

    for summary in &result.summaries {
        if let Some(filtered_summary) =
            filtered_runtime_summary_from_indices(result, summary, &criteria, true)
        {
            summaries.push(filtered_summary);
        }
    }

    for request in &result.requests {
        if !runtime_request_matches_cross_filters(request, &criteria) {
            continue;
        }

        let mut visible_sql_indices = Vec::new();
        for (sql_index, sql) in request.sql_records.iter().enumerate() {
            if !runtime_sql_matches_keyword(request, sql, &criteria) {
                continue;
            }

            visible_sql_indices.push(sql_index);
        }

        if !visible_sql_indices.is_empty() {
            sql_indices_by_request.insert(request.index, visible_sql_indices);
        }
    }

    RuntimeAnalysisFilterRows {
        filter,
        summaries: Arc::new(summaries),
        sql_indices_by_request: Arc::new(sql_indices_by_request),
    }
}

/// 按执行次数、平均耗时和 SQL 文本稳定排序 SQL 频率分析结果。
pub fn sort_runtime_sql_frequency_rows(rows: &mut [RuntimeSqlFrequencyAnalysisRow]) {
    rows.sort_by(|left, right| {
        right
            .execute_count
            .cmp(&left.execute_count)
            .then_with(|| {
                right
                    .average_execute_ms()
                    .partial_cmp(&left.average_execute_ms())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| left.normalized_sql.cmp(&right.normalized_sql))
    });
}

/// 按平均执行耗时、执行次数和 SQL 文本稳定排序慢 SQL 聚合结果。
pub fn sort_runtime_slow_sql_summary_rows(rows: &mut [RuntimeSlowSqlSummaryRow]) {
    rows.sort_by(|left, right| {
        right
            .average_execute_ms()
            .partial_cmp(&left.average_execute_ms())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.execute_count.cmp(&left.execute_count))
            .then_with(|| left.normalized_sql.cmp(&right.normalized_sql))
    });
}

/// 按执行耗时和原始位置稳定排序 SQL 频率详情结果。
pub fn sort_runtime_sql_frequency_detail_rows(rows: &mut [RuntimeSqlFrequencyDetailRow]) {
    rows.sort_by(|left, right| {
        right
            .execute_ms
            .cmp(&left.execute_ms)
            .then_with(|| left.request_index.cmp(&right.request_index))
            .then_with(|| left.sql_index.cmp(&right.sql_index))
    });
}

/// 判断请求是否命中用户名和时间区间过滤。
pub fn runtime_request_matches_cross_filters(
    request: &RuntimeRequestRecord,
    criteria: &RuntimeAnalysisFilterCriteria,
) -> bool {
    if !criteria.usernames.is_empty()
        && !criteria
            .usernames
            .iter()
            .any(|username| request.username_lowercase.contains(username))
    {
        return false;
    }
    if let Some(start_timestamp_ms) = criteria.start_timestamp_ms
        && request.request_timestamp_ms < start_timestamp_ms
    {
        return false;
    }
    if let Some(end_timestamp_ms) = criteria.end_timestamp_ms
        && request.request_timestamp_ms > end_timestamp_ms
    {
        return false;
    }
    true
}

/// 判断请求明细行是否命中关键字；SQL 命中时所属请求也视为命中。
pub fn runtime_request_matches_keyword(
    request: &RuntimeRequestRecord,
    criteria: &RuntimeAnalysisFilterCriteria,
) -> bool {
    if criteria.keyword.is_empty() {
        return true;
    }

    runtime_request_fields_match_keyword(request, criteria)
        || request
            .sql_records
            .iter()
            .any(|sql| runtime_sql_matches_keyword(request, sql, criteria))
}

/// 判断 SQL 明细行是否命中关键字；同时纳入所属请求元信息，便于跨层检索。
pub fn runtime_sql_matches_keyword(
    request: &RuntimeRequestRecord,
    sql: &RuntimeSqlRecord,
    criteria: &RuntimeAnalysisFilterCriteria,
) -> bool {
    if criteria.keyword.is_empty() {
        return true;
    }

    runtime_request_fields_match_keyword(request, criteria)
        || runtime_sql_fields_match_keyword(sql, criteria)
}

/// 判断请求字段是否命中关键字；仅在实际过滤时临时拼接，避免解析阶段预计算。
fn runtime_request_fields_match_keyword(
    request: &RuntimeRequestRecord,
    criteria: &RuntimeAnalysisFilterCriteria,
) -> bool {
    let request_text = format!(
        "{} {} {} {} {} {} {} {}",
        request.request_time_label,
        request.username,
        format_duration_for_search(request.request_duration_ms),
        request.request_path,
        request.label,
        request.path,
        format_duration_for_search(request.socket_duration_ms),
        format_duration_for_search(request.security_check_ms)
    );
    criteria.keyword_matches_text(&request_text)
}

/// 判断 SQL 字段是否命中关键字；仅在实际过滤时临时拼接，减少初次解析成本。
fn runtime_sql_fields_match_keyword(
    sql: &RuntimeSqlRecord,
    criteria: &RuntimeAnalysisFilterCriteria,
) -> bool {
    let sql_text = format!(
        "{} {} {} {} {} {} {}",
        format_duration_for_search(sql.execute_ms),
        format_duration_for_search(sql.acquire_connection_ms),
        format_duration_for_search(sql.commit_ms),
        format_duration_for_search(sql.release_connection_ms),
        format_duration_for_search(sql.parse_result_ms),
        sql.sql_text,
        sql.normalized_sql
    );
    criteria.keyword_matches_text(&sql_text)
}

/// 从原始 summary 的请求索引中应用过滤并重新计算聚合统计。
pub fn filtered_runtime_summary_from_indices(
    result: &RuntimeAnalysisResult,
    summary: &RuntimeRequestSummary,
    criteria: &RuntimeAnalysisFilterCriteria,
    apply_keyword: bool,
) -> Option<RuntimeRequestSummary> {
    let mut request_indices = summary
        .request_indices
        .iter()
        .copied()
        .filter(|index| {
            result
                .requests
                .get(*index)
                .is_some_and(|request| runtime_request_matches_cross_filters(request, criteria))
        })
        .collect::<Vec<_>>();

    // 关键字过滤会影响聚合统计本身，而不是只决定总览行是否显示。
    if apply_keyword && !criteria.keyword.is_empty() {
        let cross_filtered_summary = build_runtime_summary_from_indices(
            result,
            &summary.request_path,
            request_indices.clone(),
        )?;
        if !runtime_summary_fields_match_keyword(&cross_filtered_summary, criteria) {
            request_indices.retain(|index| {
                result
                    .requests
                    .get(*index)
                    .is_some_and(|request| runtime_request_matches_keyword(request, criteria))
            });
        }
    }

    build_runtime_summary_from_indices(result, &summary.request_path, request_indices)
}

/// 根据请求索引重新计算 Runtime 聚合行统计。
fn build_runtime_summary_from_indices(
    result: &RuntimeAnalysisResult,
    request_path: &str,
    request_indices: Vec<usize>,
) -> Option<RuntimeRequestSummary> {
    if request_indices.is_empty() {
        return None;
    }

    let request_count = request_indices.len();
    let total_duration = request_indices
        .iter()
        .filter_map(|index| result.requests.get(*index))
        .map(|request| request.request_duration_ms)
        .sum::<u64>();
    let slow_request_count = request_indices
        .iter()
        .filter_map(|index| result.requests.get(*index))
        .filter(|request| request.is_slow_sql_request)
        .count();
    let average_duration_ms = total_duration as f64 / request_count as f64;
    let slow_sql_ratio = slow_request_count as f64 / request_count as f64;

    Some(RuntimeRequestSummary {
        request_path: request_path.to_string(),
        request_count,
        average_duration_ms,
        slow_request_count,
        slow_sql_ratio,
        request_indices,
    })
}

/// 判断总览行自身可见字段是否命中关键字。
fn runtime_summary_fields_match_keyword(
    summary: &RuntimeRequestSummary,
    criteria: &RuntimeAnalysisFilterCriteria,
) -> bool {
    let summary_text = format!(
        "{} {} {} {}",
        summary.request_count,
        summary.request_path,
        format_average_duration_for_search(summary.average_duration_ms),
        format_ratio_for_search(summary.slow_sql_ratio)
    );
    criteria.keyword_matches_text(&summary_text)
}

/// 格式化整数毫秒耗时，供过滤搜索文本保持与 UI 展示一致。
fn format_duration_for_search(duration_ms: u64) -> String {
    format!("{duration_ms} ms")
}

/// 格式化平均耗时，供过滤搜索文本保持与 UI 展示一致。
fn format_average_duration_for_search(duration_ms: f64) -> String {
    format!("{duration_ms:.1} ms")
}

/// 格式化比例，供过滤搜索文本保持与 UI 展示一致。
fn format_ratio_for_search(ratio: f64) -> String {
    format!("{:.1}%", ratio * 100.0)
}

/// 将 SQL 归一化为结构模板，用于频率分析时消除常见参数值差异。
///
/// 参数说明：
/// - `sql`：Runtime 日志中记录的 SQL 原文。
///
/// 返回值：大小写和空白稳定、常见字面量替换为 `?` 的 SQL 结构文本。
pub fn normalize_runtime_sql_structure(sql: &str) -> String {
    let mut tokens = Vec::new();
    let mut chars = sql.chars().peekable();
    while let Some(ch) = chars.peek().copied() {
        if ch.is_whitespace() {
            chars.next();
            continue;
        }

        if ch == '\'' || ch == '"' {
            skip_quoted_runtime_sql_literal(&mut chars, ch);
            tokens.push("?".to_string());
            continue;
        }

        if runtime_sql_number_literal_starts(&mut chars) {
            skip_runtime_sql_number_literal(&mut chars);
            tokens.push("?".to_string());
            continue;
        }

        if runtime_sql_identifier_starts(ch) {
            let mut word = String::new();
            while let Some(next) = chars.peek().copied() {
                if runtime_sql_identifier_continues(next) {
                    word.push(next.to_ascii_lowercase());
                    chars.next();
                } else {
                    break;
                }
            }
            if matches!(word.as_str(), "true" | "false" | "null") {
                tokens.push("?".to_string());
            } else {
                tokens.push(word);
            }
            continue;
        }

        tokens.push(ch.to_string());
        chars.next();
    }

    format_runtime_sql_tokens(&collapse_runtime_sql_in_lists(tokens))
}

/// 跳过单引号或双引号包裹的 SQL 字面量，兼容 SQL 双写引号和反斜杠转义。
fn skip_quoted_runtime_sql_literal(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    quote: char,
) {
    chars.next();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let _ = chars.next();
            continue;
        }
        if ch == quote {
            if chars.peek().is_some_and(|next| *next == quote) {
                chars.next();
                continue;
            }
            break;
        }
    }
}

/// 判断当前位置是否可能是数字字面量开头，避免把标识符中的数字误归一化。
fn runtime_sql_number_literal_starts(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> bool {
    let mut cloned = chars.clone();
    match cloned.next() {
        Some(first) if first.is_ascii_digit() => true,
        Some('-' | '+') => cloned.next().is_some_and(|next| next.is_ascii_digit()),
        Some('.') => cloned.next().is_some_and(|next| next.is_ascii_digit()),
        _ => false,
    }
}

/// 跳过整数、小数、科学计数法等常见数字字面量。
fn skip_runtime_sql_number_literal(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    if chars.peek().is_some_and(|ch| matches!(ch, '-' | '+')) {
        chars.next();
    }
    while chars.peek().is_some_and(|ch| ch.is_ascii_digit()) {
        chars.next();
    }
    if chars.peek().is_some_and(|ch| *ch == '.') {
        chars.next();
        while chars.peek().is_some_and(|ch| ch.is_ascii_digit()) {
            chars.next();
        }
    }
    if chars.peek().is_some_and(|ch| matches!(ch, 'e' | 'E')) {
        let mut cloned = chars.clone();
        cloned.next();
        if cloned.peek().is_some_and(|ch| matches!(ch, '-' | '+')) {
            cloned.next();
        }
        if cloned.peek().is_some_and(|ch| ch.is_ascii_digit()) {
            chars.next();
            if chars.peek().is_some_and(|ch| matches!(ch, '-' | '+')) {
                chars.next();
            }
            while chars.peek().is_some_and(|ch| ch.is_ascii_digit()) {
                chars.next();
            }
        }
    }
}

/// 判断字符是否可以作为 SQL 标识符开头。
fn runtime_sql_identifier_starts(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

/// 判断字符是否可以继续组成 SQL 标识符。
fn runtime_sql_identifier_continues(ch: char) -> bool {
    ch == '_' || ch == '$' || ch.is_ascii_alphanumeric()
}

/// 将 `in (?, ?, ?)` 一类参数列表折叠成 `in (?)`，避免列表长度影响频率聚合。
fn collapse_runtime_sql_in_lists(tokens: Vec<String>) -> Vec<String> {
    let mut collapsed = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        if tokens[index] == "in"
            && tokens.get(index + 1).is_some_and(|token| token == "(")
            && tokens.get(index + 2).is_some_and(|token| token == "?")
        {
            let mut cursor = index + 3;
            let mut is_parameter_list = true;
            while tokens.get(cursor).is_some_and(|token| token == ",") {
                if tokens.get(cursor + 1).is_some_and(|token| token == "?") {
                    cursor += 2;
                } else {
                    is_parameter_list = false;
                    break;
                }
            }
            if is_parameter_list && tokens.get(cursor).is_some_and(|token| token == ")") {
                collapsed.extend([
                    "in".to_string(),
                    "(".to_string(),
                    "?".to_string(),
                    ")".to_string(),
                ]);
                index = cursor + 1;
                continue;
            }
        }
        collapsed.push(tokens[index].clone());
        index += 1;
    }
    collapsed
}

/// 将归一化 token 格式化为稳定单行文本。
fn format_runtime_sql_tokens(tokens: &[String]) -> String {
    let mut output = String::new();
    let mut previous: Option<&str> = None;
    for token in tokens {
        let token = token.as_str();
        if token == "," || token == ")" || token == ";" {
            while output.ends_with(' ') {
                output.pop();
            }
            output.push_str(token);
            if token == "," {
                output.push(' ');
            }
        } else if token == "(" {
            if !output.is_empty()
                && !output.ends_with(' ')
                && previous.is_some_and(|previous| previous != "(")
            {
                output.push(' ');
            }
            output.push('(');
        } else {
            if !output.is_empty() && !output.ends_with(' ') && previous != Some("(") {
                output.push(' ');
            }
            output.push_str(token);
        }
        previous = Some(token);
    }
    output.trim().to_string()
}

/// 展开分析目标；目录会递归转换成多个文件目标。
fn expand_runtime_target(
    target: RuntimeAnalysisTarget,
    loader_config: &LoaderConfig,
) -> std::result::Result<Vec<RuntimeAnalysisTarget>, (SourceId, String, String)> {
    match target.kind {
        RuntimeAnalysisTargetKind::File => Ok(vec![target]),
        RuntimeAnalysisTargetKind::Directory => {
            let SourceLocation::LocalPath(path) = &target.location else {
                return Err((
                    target.source_id,
                    target.label,
                    "Runtime 文件夹解析仅支持本地目录".to_string(),
                ));
            };
            collect_runtime_log_files(target.source_id, path, loader_config).map_err(|error| {
                (
                    target.source_id,
                    target.label.clone(),
                    format!("读取 Runtime 目录失败：{error}"),
                )
            })
        }
    }
}

/// 递归收集本地目录中的 `.log` 文件，并保持文件路径字典序稳定。
fn collect_runtime_log_files(
    source_id: SourceId,
    root: &Path,
    loader_config: &LoaderConfig,
) -> Result<Vec<RuntimeAnalysisTarget>> {
    if !root.is_dir() {
        bail!("{} 不是本地目录", root.display());
    }

    let mut paths = Vec::new();
    let mut visited_dirs = BTreeSet::new();
    collect_runtime_log_file_paths(
        root,
        loader_config.follow_symlinks,
        &mut visited_dirs,
        &mut paths,
    )?;
    paths.sort();

    Ok(paths
        .into_iter()
        .filter_map(|path| {
            let label = path.file_name()?.to_string_lossy().to_string();
            Some(RuntimeAnalysisTarget {
                source_id,
                location: SourceLocation::LocalPath(path.clone()),
                archive_probe_node: None,
                label,
                path: path.display().to_string(),
                kind: RuntimeAnalysisTargetKind::File,
                archive_passwords: ArchivePasswordStore::default(),
            })
        })
        .collect())
}

/// 深度优先收集 `.log` 文件；符号链接策略与来源加载配置保持一致。
fn collect_runtime_log_file_paths(
    dir: &Path,
    follow_symlinks: bool,
    visited_dirs: &mut BTreeSet<PathBuf>,
    paths: &mut Vec<PathBuf>,
) -> Result<()> {
    let canonical_dir = fs::canonicalize(dir)
        .with_context(|| format!("无法解析目录真实路径：{}", dir.display()))?;
    if !visited_dirs.insert(canonical_dir) {
        return Ok(());
    }

    let mut entries = fs::read_dir(dir)
        .with_context(|| format!("无法读取目录：{}", dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("无法遍历目录：{}", dir.display()))?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        let link_metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("无法读取文件元数据：{}", path.display()))?;
        let is_symlink = link_metadata.file_type().is_symlink();
        if is_symlink && !follow_symlinks {
            continue;
        }

        let metadata = if is_symlink && follow_symlinks {
            fs::metadata(&path)
        } else {
            Ok(link_metadata)
        }
        .with_context(|| format!("无法读取文件元数据：{}", path.display()))?;

        if metadata.is_dir() {
            collect_runtime_log_file_paths(&path, follow_symlinks, visited_dirs, paths)?;
        } else if metadata.is_file() && has_log_extension(&path) {
            paths.push(path);
        }
    }

    Ok(())
}

/// 判断路径是否是 `.log` 文件，大小写不敏感。
fn has_log_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("log"))
        .unwrap_or(false)
}

/// Runtime 单文件解析结果；失败时直接转换为 UI 可展示的跳过记录。
type RuntimeParseOutcome = std::result::Result<RuntimeRequestRecord, RuntimeSkippedFile>;

/// 已完成文件名和真实读取位置解析的 Runtime 文件目标。
#[derive(Clone, Debug)]
struct PreparedRuntimeTarget {
    /// 目标在来源树展开结果中的原始顺序。
    order: usize,
    /// 来源树节点 ID。
    source_id: SourceId,
    /// 文件展示名称。
    label: String,
    /// 文件路径或虚拟路径。
    path: String,
    /// 已解析出的请求元信息。
    metadata: RuntimeFileMetadata,
    /// 实际读取位置；单文件压缩包会在准备阶段解析为内部日志条目。
    location: SourceLocation,
    /// 当前会话中已输入的压缩包密码快照。
    archive_passwords: ArchivePasswordStore,
}

/// 并行读取并解析 Runtime 文件，返回顺序仍保持来源树展开后的文件顺序。
fn read_runtime_requests_parallel(
    file_targets: Vec<RuntimeAnalysisTarget>,
    default_encoding: &str,
    loader_config: &LoaderConfig,
) -> Vec<RuntimeParseOutcome> {
    let total_targets = file_targets.len();
    if total_targets == 0 {
        return Vec::new();
    }

    let mut outcomes = std::iter::repeat_with(|| None)
        .take(total_targets)
        .collect::<Vec<Option<RuntimeParseOutcome>>>();
    let mut prepared_targets = Vec::new();
    for (order, target) in file_targets.into_iter().enumerate() {
        match prepare_runtime_target(order, target, loader_config) {
            Ok(prepared) => prepared_targets.push(prepared),
            Err(skipped_file) => outcomes[order] = Some(Err(skipped_file)),
        }
    }

    let mut top_level_zip_groups = HashMap::<PathBuf, Vec<PreparedRuntimeTarget>>::new();
    let mut generic_targets = Vec::new();
    for prepared in prepared_targets {
        if let Some(archive_path) = top_level_zip_archive_path(&prepared.location)
            && prepared
                .archive_passwords
                .get(&ArchivePasswordKey::root(archive_path.clone()))
                .is_none()
        {
            top_level_zip_groups
                .entry(archive_path)
                .or_default()
                .push(prepared);
        } else {
            generic_targets.push(prepared);
        }
    }

    for (archive_path, targets) in top_level_zip_groups {
        for (order, outcome) in
            read_top_level_zip_runtime_requests(&archive_path, targets, default_encoding)
        {
            outcomes[order] = Some(outcome);
        }
    }

    for (order, outcome) in
        read_prepared_runtime_requests_parallel(generic_targets, default_encoding)
    {
        outcomes[order] = Some(outcome);
    }

    outcomes.into_iter().filter_map(|outcome| outcome).collect()
}

/// 解析 Runtime 目标的文件名和真实读取位置；失败时生成可展示跳过记录。
fn prepare_runtime_target(
    order: usize,
    target: RuntimeAnalysisTarget,
    loader_config: &LoaderConfig,
) -> std::result::Result<PreparedRuntimeTarget, RuntimeSkippedFile> {
    let source_id = target.source_id;
    let label = target.label.clone();
    let metadata = parse_runtime_file_name(&target.label).map_err(|error| RuntimeSkippedFile {
        source_id,
        label: label.clone(),
        reason: error.to_string(),
    })?;
    let location = resolve_runtime_target_location(&target, loader_config).map_err(|error| {
        RuntimeSkippedFile {
            source_id,
            label: label.clone(),
            reason: error.to_string(),
        }
    })?;

    Ok(PreparedRuntimeTarget {
        order,
        source_id,
        label: target.label,
        path: target.path,
        metadata,
        location,
        archive_passwords: target.archive_passwords,
    })
}

/// 返回可批量读取的顶层 ZIP 路径；嵌套压缩包暂回退到通用读取路径。
fn top_level_zip_archive_path(location: &SourceLocation) -> Option<PathBuf> {
    let SourceLocation::ArchiveEntry {
        archive_path,
        root_format,
        container_entries,
        ..
    } = location
    else {
        return None;
    };
    (*root_format == ArchiveFormat::Zip && container_entries.is_empty())
        .then(|| archive_path.clone())
}

/// 并行读取非顶层 ZIP 批量路径的 Runtime 文件。
fn read_prepared_runtime_requests_parallel(
    prepared_targets: Vec<PreparedRuntimeTarget>,
    default_encoding: &str,
) -> Vec<(usize, RuntimeParseOutcome)> {
    if prepared_targets.is_empty() {
        return Vec::new();
    }

    let worker_count = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(MAX_RUNTIME_PARSE_WORKERS)
        .min(prepared_targets.len())
        .max(1);
    if worker_count == 1 {
        let mut encoding_hint = None;
        return prepared_targets
            .into_iter()
            .map(|target| {
                let order = target.order;
                (
                    order,
                    read_prepared_runtime_request_outcome(
                        target,
                        default_encoding,
                        &mut encoding_hint,
                    ),
                )
            })
            .collect();
    }

    let chunk_size = prepared_targets.len().div_ceil(worker_count).max(1);
    let mut ordered_results = Vec::with_capacity(prepared_targets.len());
    thread::scope(|scope| {
        let mut handles = Vec::new();
        for chunk in prepared_targets.chunks(chunk_size) {
            handles.push(scope.spawn(move || {
                let mut encoding_hint = None;
                chunk
                    .iter()
                    .cloned()
                    .map(|target| {
                        let order = target.order;
                        (
                            order,
                            read_prepared_runtime_request_outcome(
                                target,
                                default_encoding,
                                &mut encoding_hint,
                            ),
                        )
                    })
                    .collect::<Vec<_>>()
            }));
        }

        for handle in handles {
            let mut worker_results = handle.join().expect("Runtime 解析工作线程不应 panic");
            ordered_results.append(&mut worker_results);
        }
    });
    ordered_results.sort_by_key(|(order, _)| *order);
    ordered_results
}

/// 读取已准备好的 Runtime 文件，并把错误包装为跳过记录，便于并行结果统一合并。
fn read_prepared_runtime_request_outcome(
    target: PreparedRuntimeTarget,
    default_encoding: &str,
    encoding_hint: &mut Option<String>,
) -> RuntimeParseOutcome {
    let source_id = target.source_id;
    let label = target.label.clone();
    read_prepared_runtime_request(target, default_encoding, encoding_hint).map_err(|error| {
        RuntimeSkippedFile {
            source_id,
            label,
            reason: error.to_string(),
        }
    })
}

/// 批量读取同一个顶层 ZIP 中的 Runtime 日志，避免每个条目重复打开压缩包。
fn read_top_level_zip_runtime_requests(
    archive_path: &Path,
    targets: Vec<PreparedRuntimeTarget>,
    default_encoding: &str,
) -> Vec<(usize, RuntimeParseOutcome)> {
    if targets.len() >= MIN_PARALLEL_ZIP_TARGETS {
        let worker_count = thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
            .min(MAX_RUNTIME_PARSE_WORKERS)
            .min(targets.len())
            .max(1);
        if worker_count > 1 {
            let chunk_size = targets.len().div_ceil(worker_count).max(1);
            let mut ordered_results = Vec::with_capacity(targets.len());
            thread::scope(|scope| {
                let mut handles = Vec::new();
                for chunk in targets.chunks(chunk_size) {
                    handles.push(scope.spawn(move || {
                        read_top_level_zip_runtime_requests_sequential(
                            archive_path,
                            chunk.to_vec(),
                            default_encoding,
                        )
                    }));
                }

                for handle in handles {
                    let mut worker_results =
                        handle.join().expect("Runtime ZIP 解析工作线程不应 panic");
                    ordered_results.append(&mut worker_results);
                }
            });
            ordered_results.sort_by_key(|(order, _)| *order);
            return ordered_results;
        }
    }

    read_top_level_zip_runtime_requests_sequential(archive_path, targets, default_encoding)
}

/// 在当前线程内批量读取同一个顶层 ZIP 的一组 Runtime 日志。
fn read_top_level_zip_runtime_requests_sequential(
    archive_path: &Path,
    targets: Vec<PreparedRuntimeTarget>,
    default_encoding: &str,
) -> Vec<(usize, RuntimeParseOutcome)> {
    let file = match fs::File::open(archive_path)
        .with_context(|| format!("无法打开 ZIP 压缩包：{}", archive_path.display()))
    {
        Ok(file) => file,
        Err(error) => {
            return targets
                .into_iter()
                .map(|target| {
                    (
                        target.order,
                        Err(RuntimeSkippedFile {
                            source_id: target.source_id,
                            label: target.label,
                            reason: error.to_string(),
                        }),
                    )
                })
                .collect();
        }
    };
    let mut archive = match ZipArchive::new(file)
        .with_context(|| format!("无法解析 ZIP 压缩包：{}", archive_path.display()))
    {
        Ok(archive) => archive,
        Err(error) => {
            return targets
                .into_iter()
                .map(|target| {
                    (
                        target.order,
                        Err(RuntimeSkippedFile {
                            source_id: target.source_id,
                            label: target.label,
                            reason: error.to_string(),
                        }),
                    )
                })
                .collect();
        }
    };

    let entry_index = build_zip_entry_index(&mut archive);
    let mut encoding_hint = None;
    targets
        .into_iter()
        .map(|target| {
            let order = target.order;
            let outcome = read_prepared_runtime_request_from_zip(
                &mut archive,
                &entry_index,
                target,
                default_encoding,
                &mut encoding_hint,
            );
            (order, outcome)
        })
        .collect()
}

/// 从已打开的 ZIP 压缩包中读取单条 Runtime 日志并解析。
fn read_prepared_runtime_request_from_zip<R>(
    archive: &mut ZipArchive<R>,
    entry_index: &HashMap<String, usize>,
    target: PreparedRuntimeTarget,
    default_encoding: &str,
    encoding_hint: &mut Option<String>,
) -> RuntimeParseOutcome
where
    R: Read + Seek,
{
    let source_id = target.source_id;
    let label = target.label.clone();
    read_prepared_runtime_request_from_zip_inner(
        archive,
        entry_index,
        target,
        default_encoding,
        encoding_hint,
    )
    .map_err(|error| RuntimeSkippedFile {
        source_id,
        label,
        reason: error.to_string(),
    })
}

/// 执行已打开 ZIP 中单个 Runtime 条目的读取和解析。
fn read_prepared_runtime_request_from_zip_inner<R>(
    archive: &mut ZipArchive<R>,
    entry_index: &HashMap<String, usize>,
    target: PreparedRuntimeTarget,
    default_encoding: &str,
    encoding_hint: &mut Option<String>,
) -> Result<RuntimeRequestRecord>
where
    R: Read + Seek,
{
    let SourceLocation::ArchiveEntry { entry_path, .. } = &target.location else {
        bail!("Runtime ZIP 批量读取收到非压缩条目：{}", target.path);
    };
    let bytes = read_zip_entry_bytes_from_open_archive(
        archive,
        entry_index,
        entry_path,
        &target.location.display_path(),
    )?;
    let sql_records = parse_runtime_sql_records_from_bytes(&bytes, default_encoding, encoding_hint);

    Ok(build_request_record(
        0,
        target.source_id,
        target.label,
        target.path,
        target.metadata,
        sql_records,
    ))
}

/// 为当前打开的 ZIP 建立归一化条目路径索引，避免每个 Runtime 条目读取时重复线性扫描。
fn build_zip_entry_index<R>(archive: &mut ZipArchive<R>) -> HashMap<String, usize>
where
    R: Read + Seek,
{
    let mut index = HashMap::with_capacity(archive.len());
    for entry_index in 0..archive.len() {
        if let Ok(file) = archive.by_index(entry_index) {
            index
                .entry(normalize_archive_entry_path(file.name()))
                .or_insert(entry_index);
        }
    }
    index
}

/// 从已打开的 ZIP 中读取条目字节，优先按中央目录名称直接定位，失败后再归一化扫描兼容异常路径。
fn read_zip_entry_bytes_from_open_archive<R>(
    archive: &mut ZipArchive<R>,
    entry_index: &HashMap<String, usize>,
    entry_path: &str,
    source_label: &str,
) -> Result<Vec<u8>>
where
    R: Read + Seek,
{
    let normalized_entry_path = normalize_archive_entry_path(entry_path);
    if let Ok(mut file) = archive.by_name(&normalized_entry_path) {
        if file.is_dir() {
            bail!("ZIP 条目是目录，无法读取内容：{normalized_entry_path}");
        }
        let mut bytes = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut bytes).with_context(|| {
            format!("无法读取 ZIP 条目内容 {normalized_entry_path}：{source_label}")
        })?;
        return Ok(bytes);
    }

    if let Some(index) = entry_index.get(&normalized_entry_path).copied() {
        let mut file = archive
            .by_index(index)
            .with_context(|| format!("无法读取 ZIP 第 {index} 个条目：{source_label}"))?;
        if file.is_dir() {
            bail!("ZIP 条目是目录，无法读取内容：{normalized_entry_path}");
        }

        let mut bytes = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut bytes).with_context(|| {
            format!("无法读取 ZIP 条目内容 {normalized_entry_path}：{source_label}")
        })?;
        return Ok(bytes);
    }

    bail!("无法读取 ZIP 条目 {normalized_entry_path}：{source_label}")
}

/// 读取一个已准备好的 Runtime 文件并解析为请求记录。
fn read_prepared_runtime_request(
    target: PreparedRuntimeTarget,
    default_encoding: &str,
    encoding_hint: &mut Option<String>,
) -> Result<RuntimeRequestRecord> {
    let sql_records = read_runtime_sql_records_from_location(
        &target.location,
        default_encoding,
        encoding_hint,
        &target.archive_passwords,
    )
    .with_context(|| format!("读取 Runtime 日志失败：{}", target.location.display_path()))?;

    Ok(build_request_record(
        0,
        target.source_id,
        target.label,
        target.path,
        target.metadata,
        sql_records,
    ))
}

/// 直接读取 Runtime 日志原始字节并解析 SQL，避免构建通用日志展示文档的额外开销。
fn read_runtime_sql_records_from_location(
    location: &SourceLocation,
    default_encoding: &str,
    encoding_hint: &mut Option<String>,
    archive_passwords: &ArchivePasswordStore,
) -> Result<Vec<RuntimeSqlRecord>> {
    let bytes = match location {
        SourceLocation::LocalPath(path) => {
            if should_stream_runtime_utf8_file(default_encoding, encoding_hint)
                && let Ok(records) = read_runtime_sql_records_from_utf8_file(path)
            {
                *encoding_hint = Some("UTF-8".to_string());
                return Ok(records);
            }
            fs::read(path)
                .with_context(|| format!("无法读取 Runtime 日志文件：{}", path.display()))?
        }
        SourceLocation::ArchiveEntry { .. } => {
            ArchiveStreamBackend::read_to_bytes(location, archive_passwords)?
        }
    };
    Ok(parse_runtime_sql_records_from_bytes(
        &bytes,
        default_encoding,
        encoding_hint,
    ))
}

/// 判断当前批次是否可以尝试 UTF-8 流式解析，失败后仍会回退到完整解码。
fn should_stream_runtime_utf8_file(default_encoding: &str, encoding_hint: &Option<String>) -> bool {
    encoding_hint
        .as_deref()
        .map(|encoding| encoding.eq_ignore_ascii_case("UTF-8"))
        .unwrap_or_else(|| default_encoding.eq_ignore_ascii_case("UTF-8"))
}

/// 使用 UTF-8 流式读取本地 Runtime 日志，避免为大量小文件构造完整正文字符串。
fn read_runtime_sql_records_from_utf8_file(path: &Path) -> Result<Vec<RuntimeSqlRecord>> {
    let file = fs::File::open(path)
        .with_context(|| format!("无法打开 Runtime 日志文件：{}", path.display()))?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut parser = RuntimeSqlParser::default();
    let mut line = String::new();
    let mut is_first_line = true;

    loop {
        line.clear();
        let bytes_read = reader
            .read_line(&mut line)
            .with_context(|| format!("无法按 UTF-8 读取 Runtime 日志：{}", path.display()))?;
        if bytes_read == 0 {
            break;
        }

        let trimmed = line.trim_end_matches(['\n', '\r']);
        let content = if is_first_line {
            is_first_line = false;
            trimmed.strip_prefix('\u{FEFF}').unwrap_or(trimmed)
        } else {
            trimmed
        };
        parser.push_line(content);
    }

    Ok(parser.finish())
}

/// 使用批次内的编码提示解码 Runtime 日志，并在成功后更新提示以加速同批后续文件。
fn parse_runtime_sql_records_from_bytes(
    bytes: &[u8],
    default_encoding: &str,
    encoding_hint: &mut Option<String>,
) -> Vec<RuntimeSqlRecord> {
    let decoded = if let Some(encoding_label) = encoding_hint.as_deref() {
        decode_log_bytes_with_known_encoding(bytes, encoding_label, default_encoding)
    } else {
        decode_log_bytes(bytes, default_encoding)
    };
    if !decoded.encoding_label.eq_ignore_ascii_case("UTF-8-lossy") {
        *encoding_hint = Some(decoded.encoding_label.clone());
    }
    parse_runtime_sql_records(&decoded.text)
}

/// 解析 Runtime 输入目标的真实读取位置；待探测压缩包会独立判断是否为单文件日志。
fn resolve_runtime_target_location(
    target: &RuntimeAnalysisTarget,
    loader_config: &LoaderConfig,
) -> Result<SourceLocation> {
    let Some(node) = target.archive_probe_node.clone() else {
        return Ok(target.location.clone());
    };

    let probe_result = LogSourceLoader::new(loader_config.clone())
        .with_archive_passwords(target.archive_passwords.clone())
        .probe_archive_nodes(vec![SourceArchiveProbeRequest {
            source_id: target.source_id,
            node,
        }])
        .into_iter()
        .next()
        .and_then(|result| result.patch)
        .ok_or_else(|| anyhow!("压缩包根层不是单文件日志，请展开后选择具体日志条目"))?;

    Ok(probe_result.location)
}

/// 从文件名中解析请求元信息。
fn parse_runtime_file_name(label: &str) -> Result<RuntimeFileMetadata> {
    let stem = Path::new(label)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| anyhow!("文件名缺少有效 Runtime 元信息"))?;
    let parts = stem.split('&').collect::<Vec<_>>();
    if parts.len() != 6 {
        bail!("文件名不符合 Runtime 格式");
    }

    let request_duration_ms = parse_unsigned_field(parts[0], "请求耗时")?;
    let username = parts[1].to_string();
    let request_path = runtime_request_path_from_api(parts[2]);
    if request_path.is_empty() {
        bail!("文件名缺少请求 API");
    }
    let request_timestamp_ms = parts[3]
        .parse::<i64>()
        .with_context(|| format!("请求时间戳不是有效毫秒值：{}", parts[3]))?;
    let socket_duration_ms = parse_unsigned_field(parts[4], "socket 耗时")?;
    let security_check_ms = parse_unsigned_field(parts[5], "安全校验耗时")?;

    Ok(RuntimeFileMetadata {
        username,
        request_path,
        request_duration_ms,
        request_timestamp_ms,
        socket_duration_ms,
        security_check_ms,
    })
}

/// 将文件名中的接口字段转换为展示请求地址。
fn runtime_request_path_from_api(raw_api: &str) -> String {
    raw_api.replace('_', "/")
}

/// 解析文件名中的无符号毫秒字段。
fn parse_unsigned_field(raw: &str, field_label: &str) -> Result<u64> {
    raw.parse::<u64>()
        .with_context(|| format!("{field_label}不是有效毫秒值：{raw}"))
}

/// Runtime 文件名中的请求元信息。
#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimeFileMetadata {
    /// 用户名。
    username: String,
    /// 请求地址。
    request_path: String,
    /// 请求总耗时。
    request_duration_ms: u64,
    /// 请求时间戳。
    request_timestamp_ms: i64,
    /// socket 耗时。
    socket_duration_ms: u64,
    /// 安全校验耗时。
    security_check_ms: u64,
}

/// 根据元信息和 SQL 明细构造请求记录。
fn build_request_record(
    index: usize,
    source_id: SourceId,
    label: String,
    path: String,
    metadata: RuntimeFileMetadata,
    sql_records: Vec<RuntimeSqlRecord>,
) -> RuntimeRequestRecord {
    let sql_total_execute_ms = sql_records
        .iter()
        .map(|record| record.execute_ms)
        .sum::<u64>();
    let is_slow_sql_request = metadata.request_duration_ms > 0
        && sql_total_execute_ms.saturating_mul(100)
            > metadata
                .request_duration_ms
                .saturating_mul(SLOW_SQL_REQUEST_PERCENT);
    let username = metadata.username;
    let username_lowercase = username.to_lowercase();
    let request_path = metadata.request_path;
    let request_time_label = format_request_time_millis(metadata.request_timestamp_ms);

    RuntimeRequestRecord {
        index,
        source_id,
        label,
        path,
        username,
        username_lowercase,
        request_path,
        request_duration_ms: metadata.request_duration_ms,
        request_timestamp_ms: metadata.request_timestamp_ms,
        request_time_label,
        socket_duration_ms: metadata.socket_duration_ms,
        security_check_ms: metadata.security_check_ms,
        sql_records,
        sql_total_execute_ms,
        is_slow_sql_request,
    }
}

/// 将毫秒时间戳格式化为本地时间。
fn format_request_time_millis(timestamp_ms: i64) -> String {
    Local
        .timestamp_millis_opt(timestamp_ms)
        .single()
        .map(|datetime| datetime.format("%Y-%m-%d %H:%M:%S%.3f").to_string())
        .unwrap_or_else(|| timestamp_ms.to_string())
}

/// Runtime SQL 文本增量解析器。
#[derive(Debug, Default)]
struct RuntimeSqlParser {
    /// 已完成解析的 SQL 明细。
    records: Vec<RuntimeSqlRecord>,
    /// 当前正在收集的 SQL 明细。
    current: Option<RuntimeSqlRecord>,
}

impl RuntimeSqlParser {
    /// 追加一行 Runtime 日志内容。
    fn push_line(&mut self, line: &str) {
        if let Some(record) = parse_runtime_sql_start_line(line) {
            self.flush_current();
            self.current = Some(record);
            return;
        }

        // 不符合 5 个耗时前缀的行视为上一条 SQL 的换行续写；文件开头的杂散行直接忽略。
        if let Some(current) = self.current.as_mut() {
            if !current.sql_text.is_empty() {
                current.sql_text.push('\n');
            }
            current.sql_text.push_str(line);
        }
    }

    /// 完成解析并返回 SQL 明细。
    fn finish(mut self) -> Vec<RuntimeSqlRecord> {
        self.flush_current();
        self.records
    }

    /// 把当前 SQL 明细写入结果。
    fn flush_current(&mut self) {
        if let Some(mut record) = self.current.take() {
            record.normalized_sql = normalize_runtime_sql_structure(&record.sql_text);
            self.records.push(record);
        }
    }
}

/// 尝试把一行解析为新的 SQL 明细起始行。
fn parse_runtime_sql_start_line(line: &str) -> Option<RuntimeSqlRecord> {
    let (execute_ms, rest) = parse_duration_token(line.trim_start())?;
    let (acquire_connection_ms, rest) = parse_duration_token(rest.trim_start())?;
    let (commit_ms, rest) = parse_duration_token(rest.trim_start())?;
    let (release_connection_ms, rest) = parse_duration_token(rest.trim_start())?;
    let (parse_result_ms, rest) = parse_duration_token(rest.trim_start())?;

    Some(RuntimeSqlRecord {
        execute_ms,
        acquire_connection_ms,
        commit_ms,
        release_connection_ms,
        parse_result_ms,
        sql_text: rest.trim_start().to_string(),
        normalized_sql: String::new(),
    })
}

/// 解析形如 `12ms` 或裸数字 `12` 的毫秒耗时 token，并返回剩余字符串。
fn parse_duration_token(text: &str) -> Option<(u64, &str)> {
    let token_end = text
        .find(|character: char| character.is_ascii_whitespace())
        .unwrap_or(text.len());
    let token = &text[..token_end];
    let millis_text = token.strip_suffix("ms").unwrap_or(token);
    let millis = millis_text.parse::<u64>().ok()?;
    Some((millis, &text[token_end..]))
}

/// 解析完整文本中的 SQL 明细。
fn parse_runtime_sql_records(text: &str) -> Vec<RuntimeSqlRecord> {
    let mut parser = RuntimeSqlParser::default();
    for line in text.lines() {
        parser.push_line(line);
    }
    parser.finish()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::loader::SourceLocation;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    use super::*;

    /// 测试临时目录序号，避免并发测试复用同一路径。
    static NEXT_TEST_DIR_ID: AtomicUsize = AtomicUsize::new(0);

    /// 创建 Runtime 分析测试使用的隔离临时目录。
    fn runtime_test_dir(label: &str) -> PathBuf {
        let id = NEXT_TEST_DIR_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "argus-runtime-analysis-{label}-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("应能创建 Runtime 测试目录");
        dir
    }

    /// 验证标准文件名会被解析为请求元信息，且 API 下划线会转为斜杠。
    #[test]
    fn parses_runtime_file_name() {
        let request = parse_runtime_request_text(
            SourceId(1),
            "6007&wangyx&_api_workflow_reqform_requestOperation&1782368843095&0&153.log",
            "/tmp/runtime.log",
            "1ms 0ms 0ms 0ms 0ms select 1",
        )
        .expect("应能解析 Runtime 文件");

        assert_eq!(request.username, "wangyx");
        assert_eq!(
            request.request_path,
            "/api/workflow/reqform/requestOperation"
        );
        assert_eq!(request.request_duration_ms, 6007);
        assert_eq!(request.socket_duration_ms, 0);
        assert_eq!(request.security_check_ms, 153);
        assert_eq!(request.sql_records.len(), 1);
    }

    /// 验证文件名允许空用户名，但必须保留 6 个字段。
    #[test]
    fn parses_empty_username() {
        let request = parse_runtime_request_text(
            SourceId(1),
            "12274&&_join_apmagent.jsp&1782368723387&1&2.log",
            "/tmp/runtime.log",
            "",
        )
        .expect("空用户名是合法 Runtime 文件名");

        assert_eq!(request.username, "");
        assert_eq!(request.request_path, "/join/apmagent.jsp");
        assert!(request.sql_records.is_empty());
    }

    /// 验证坏文件名会被跳过而不是产生错误数据。
    #[test]
    fn rejects_bad_file_name() {
        let error =
            parse_runtime_request_text(SourceId(1), "plain.log", "/tmp/plain.log", "").unwrap_err();

        assert!(error.to_string().contains("Runtime 格式"));
    }

    /// 验证 SQL 起始行和换行 SQL 文本会合并为同一条记录。
    #[test]
    fn parses_multiline_sql_records() {
        let request = parse_runtime_request_text(
            SourceId(1),
            "100&user&_api_demo&1782368843095&0&0.log",
            "/tmp/runtime.log",
            "1ms 2ms 3ms 4ms 5ms select *\nfrom table_a\n6ms 0ms 0ms 0ms 0ms update table_b",
        )
        .expect("应能解析多行 SQL");

        assert_eq!(request.sql_records.len(), 2);
        assert_eq!(request.sql_records[0].execute_ms, 1);
        assert_eq!(request.sql_records[0].sql_text, "select *\nfrom table_a");
        assert_eq!(
            request.sql_records[0].normalized_sql,
            "select * from table_a"
        );
        assert_eq!(request.sql_records[1].sql_text, "update table_b");
    }

    /// 验证文件开头的非 SQL 行会被忽略，缺失字段的行不会误判为 SQL。
    #[test]
    fn ignores_orphan_and_incomplete_lines() {
        let request = parse_runtime_request_text(
            SourceId(1),
            "100&user&_api_demo&1782368843095&0&0.log",
            "/tmp/runtime.log",
            "header\n1ms 2ms 3ms 4ms select missing\n7ms 0ms 0ms 0ms 0ms select ok",
        )
        .expect("应能忽略异常行");

        assert_eq!(request.sql_records.len(), 1);
        assert_eq!(request.sql_records[0].sql_text, "select ok");
    }

    /// 验证慢 SQL 请求按 SQL 累积耗时是否超过请求总耗时 90% 计算。
    #[test]
    fn marks_slow_sql_request_by_cumulative_sql_ratio() {
        let slow = parse_runtime_request_text(
            SourceId(1),
            "100&user&_api_demo&1782368843095&0&0.log",
            "/tmp/slow.log",
            "91ms 0ms 0ms 0ms 0ms select slow",
        )
        .expect("应能解析慢请求");
        let fast = parse_runtime_request_text(
            SourceId(2),
            "100&user&_api_demo&1782368843096&0&0.log",
            "/tmp/fast.log",
            "90ms 0ms 0ms 0ms 0ms select not slow",
        )
        .expect("应能解析普通请求");

        assert!(slow.is_slow_sql_request);
        assert!(!fast.is_slow_sql_request);
    }

    /// 验证相同请求地址会合并，并计算平均耗时和慢 SQL 比例。
    #[test]
    fn aggregates_requests_by_request_path() {
        let mut first = parse_runtime_request_text(
            SourceId(1),
            "100&alice&_api_demo&1782368843095&0&0.log",
            "/tmp/one.log",
            "91ms 0ms 0ms 0ms 0ms select slow",
        )
        .expect("应能解析第一条请求");
        let mut second = parse_runtime_request_text(
            SourceId(2),
            "300&bob&_api_demo&1782368843096&0&0.log",
            "/tmp/two.log",
            "1ms 0ms 0ms 0ms 0ms select fast",
        )
        .expect("应能解析第二条请求");
        first.index = 0;
        second.index = 1;

        let result = build_runtime_analysis_result(vec![first, second], Vec::new(), 2);

        assert_eq!(result.summaries.len(), 1);
        assert_eq!(result.summaries[0].request_count, 2);
        assert_eq!(result.summaries[0].average_duration_ms, 200.0);
        assert_eq!(result.summaries[0].slow_request_count, 1);
        assert_eq!(result.summaries[0].slow_sql_ratio, 0.5);
        assert_eq!(result.total_sql_records, 2);
        assert_eq!(
            build_runtime_slow_sql_rows_for_filter(
                &result,
                &RuntimeAnalysisFilterSnapshot::default()
            )[0]
            .average_execute_ms(),
            91.0
        );
    }

    /// 验证 SQL 归一化会消除常见参数差异，并保留可读的结构文本。
    #[test]
    fn normalizes_runtime_sql_literals_to_structure() {
        let first =
            normalize_runtime_sql_structure("SELECT * FROM user WHERE id = 1 AND name = 'Alice'");
        let second =
            normalize_runtime_sql_structure("select  *  from user where id = 2 and name = 'Bob'");

        assert_eq!(first, "select * from user where id = ? and name = ?");
        assert_eq!(first, second);
    }

    /// 验证布尔、空值、科学计数法和 IN 参数列表不会干扰 SQL 频率聚合。
    #[test]
    fn normalizes_runtime_sql_common_parameter_shapes() {
        let normalized = normalize_runtime_sql_structure(
            "select * from orders where enabled = true and deleted_at is null and price > -1.5e3 and id in (1, 2, 3)",
        );

        assert_eq!(
            normalized,
            "select * from orders where enabled = ? and deleted_at is ? and price > ? and id in (?)"
        );
    }

    /// 验证 Runtime 关键字匹配按需构造搜索文本，不依赖解析阶段预计算字段。
    #[test]
    fn runtime_filter_keyword_matches_without_parse_time_search_text() {
        let request = parse_runtime_request_text(
            SourceId(1),
            "120&ALICE&_api_demo&1782368843095&3&4.log",
            "/tmp/runtime.log",
            "15ms 1ms 2ms 3ms 4ms SELECT * FROM users WHERE id = 42",
        )
        .expect("应能解析 Runtime 日志");

        assert!(request.username_lowercase.contains("alice"));
        let criteria = parse_runtime_analysis_filter_criteria(&RuntimeAnalysisFilterSnapshot {
            keyword: "id = ?".to_string(),
            username: String::new(),
            start_time: String::new(),
            end_time: String::new(),
        });
        assert!(runtime_sql_matches_keyword(
            &request,
            &request.sql_records[0],
            &criteria
        ));
    }

    /// 验证统一过滤缓存只产出统计和可见 SQL 索引，SQL 分析结果改为按需构建。
    #[test]
    fn builds_shared_runtime_filter_rows_without_sql_analysis_rows() {
        let mut first = parse_runtime_request_text(
            SourceId(1),
            "100&alice&_api_sql&1000&0&0.log",
            "/tmp/sql-a.log",
            "10 0 0 0 0 select * from users where id = 1",
        )
        .expect("应能解析第一条 SQL 测试日志");
        let mut second = parse_runtime_request_text(
            SourceId(2),
            "100&alice&_api_sql&2000&0&0.log",
            "/tmp/sql-b.log",
            "30 0 0 0 0 select * from users where id = 2",
        )
        .expect("应能解析第二条 SQL 测试日志");
        let mut third = parse_runtime_request_text(
            SourceId(3),
            "100&bob&_api_sql&3000&0&0.log",
            "/tmp/sql-c.log",
            "50 0 0 0 0 select * from orders where status = 'PAID'",
        )
        .expect("应能解析第三条 SQL 测试日志");
        first.index = 0;
        second.index = 1;
        third.index = 2;
        let result = build_runtime_analysis_result(vec![first, second, third], Vec::new(), 3);

        let rows = build_runtime_analysis_filter_rows(
            &result,
            RuntimeAnalysisFilterSnapshot {
                keyword: "id = 2".to_string(),
                username: "alice".to_string(),
                start_time: String::new(),
                end_time: String::new(),
            },
        );

        assert_eq!(rows.summaries.len(), 1);
        assert_eq!(rows.summaries[0].request_count, 1);
        assert_eq!(
            rows.sql_indices_by_request
                .get(&1)
                .cloned()
                .unwrap_or_default(),
            vec![0]
        );
        let filter = RuntimeAnalysisFilterSnapshot {
            keyword: "id = 2".to_string(),
            username: "alice".to_string(),
            start_time: String::new(),
            end_time: String::new(),
        };
        assert_eq!(
            build_runtime_sql_frequency_rows_for_filter(&result, &filter)[0].execute_count,
            1
        );
        assert_eq!(
            build_runtime_slow_sql_rows_for_filter(&result, &filter)[0].average_execute_ms(),
            30.0
        );
    }

    /// 验证本地 UTF-8 Runtime 文件可以走流式逐行解析快路径。
    #[test]
    fn streams_utf8_runtime_file_lines() {
        let dir = runtime_test_dir("stream_utf8");
        let path = dir.join("100&u&_api_stream&1782368843095&0&0.log");
        fs::write(
            &path,
            "\u{FEFF}1ms 0ms 0ms 0ms 0ms select *\nfrom table_a\n2ms 0ms 0ms 0ms 0ms select 2",
        )
        .expect("应能写入 UTF-8 Runtime 日志");
        let mut encoding_hint = None;

        let records = read_runtime_sql_records_from_location(
            &SourceLocation::LocalPath(path),
            "UTF-8",
            &mut encoding_hint,
            &ArchivePasswordStore::default(),
        )
        .expect("应能流式读取 Runtime 日志");

        assert_eq!(encoding_hint.as_deref(), Some("UTF-8"));
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].sql_text, "select *\nfrom table_a");
        assert_eq!(records[1].sql_text, "select 2");
    }

    /// 验证本地目录递归会收集子目录中的 `.log` 文件，并跳过其他扩展名。
    #[test]
    fn expands_local_directory_recursively() {
        let dir = runtime_test_dir("recursive");
        let child_dir = dir.join("child");
        fs::create_dir(&child_dir).expect("应能创建子目录");
        fs::write(
            dir.join("100&u&_api_one&1782368843095&0&0.log"),
            "1ms 0ms 0ms 0ms 0ms select 1",
        )
        .expect("应能写入根日志");
        fs::write(
            child_dir.join("200&u&_api_two&1782368843096&0&0.log"),
            "1ms 0ms 0ms 0ms 0ms select 2",
        )
        .expect("应能写入子日志");
        fs::write(child_dir.join("ignore.txt"), "ignore").expect("应能写入非日志");

        let targets = collect_runtime_log_files(SourceId(7), &dir, &LoaderConfig::default())
            .expect("应能递归收集日志");

        assert_eq!(targets.len(), 2);
        assert!(
            targets
                .iter()
                .all(|target| target.kind == RuntimeAnalysisTargetKind::File)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// 验证开启跟随符号链接时，目录回环不会导致 Runtime 递归扫描卡死。
    #[cfg(unix)]
    #[test]
    fn runtime_directory_recursion_skips_symlink_cycles() {
        let dir = runtime_test_dir("symlink-cycle");
        fs::write(
            dir.join("100&u&_api_one&1782368843095&0&0.log"),
            "1ms 0ms 0ms 0ms 0ms select 1",
        )
        .expect("应能写入 Runtime 日志");
        std::os::unix::fs::symlink(&dir, dir.join("loop")).expect("应能创建目录符号链接回环");
        let mut config = LoaderConfig::default();
        config.follow_symlinks = true;

        let targets = collect_runtime_log_files(SourceId(8), &dir, &config)
            .expect("应能在符号链接回环中完成收集");

        assert_eq!(targets.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    /// 验证 GBK 编码文件可以通过统一读取器自动识别并解析中文 SQL。
    #[test]
    fn analyzes_gbk_encoded_runtime_file() {
        let dir = runtime_test_dir("gbk");
        let path = dir.join("100&u&_api_demo&1782368843095&0&0.log");
        let (bytes, _, _) = encoding_rs::GBK.encode("1ms 0ms 0ms 0ms 0ms select '中文'");
        let mut file = fs::File::create(&path).expect("应能创建 GBK 日志");
        file.write_all(&bytes).expect("应能写入 GBK 日志");

        let result = analyze_runtime_targets(
            vec![RuntimeAnalysisTarget {
                source_id: SourceId(1),
                location: SourceLocation::LocalPath(path.clone()),
                archive_probe_node: None,
                label: path.file_name().unwrap().to_string_lossy().to_string(),
                path: path.display().to_string(),
                kind: RuntimeAnalysisTargetKind::File,
                archive_passwords: ArchivePasswordStore::default(),
            }],
            "UTF-8".to_string(),
            LoaderConfig::default(),
        );

        assert_eq!(result.requests.len(), 1);
        assert!(result.requests[0].sql_records[0].sql_text.contains("中文"));
        let _ = fs::remove_dir_all(&dir);
    }

    /// 验证并行解析不会改变来源树展开后的请求顺序。
    #[test]
    fn parallel_runtime_analysis_keeps_source_order() {
        let dir = runtime_test_dir("parallel-order");
        let first = dir.join("100&u&_api_first&1782368843095&0&0.log");
        let second = dir.join("200&u&_api_second&1782368843096&0&0.log");
        fs::write(&first, "1ms 0ms 0ms 0ms 0ms select first").expect("应能写入第一条日志");
        fs::write(&second, "1ms 0ms 0ms 0ms 0ms select second").expect("应能写入第二条日志");

        let result = analyze_runtime_targets(
            vec![
                RuntimeAnalysisTarget {
                    source_id: SourceId(1),
                    location: SourceLocation::LocalPath(second.clone()),
                    archive_probe_node: None,
                    label: second.file_name().unwrap().to_string_lossy().to_string(),
                    path: second.display().to_string(),
                    kind: RuntimeAnalysisTargetKind::File,
                    archive_passwords: ArchivePasswordStore::default(),
                },
                RuntimeAnalysisTarget {
                    source_id: SourceId(2),
                    location: SourceLocation::LocalPath(first.clone()),
                    archive_probe_node: None,
                    label: first.file_name().unwrap().to_string_lossy().to_string(),
                    path: first.display().to_string(),
                    kind: RuntimeAnalysisTargetKind::File,
                    archive_passwords: ArchivePasswordStore::default(),
                },
            ],
            "UTF-8".to_string(),
            LoaderConfig::default(),
        );

        assert_eq!(result.requests.len(), 2);
        assert_eq!(result.requests[0].request_path, "/api/second");
        assert_eq!(result.requests[0].index, 0);
        assert_eq!(result.requests[1].request_path, "/api/first");
        assert_eq!(result.requests[1].index, 1);
        let _ = fs::remove_dir_all(&dir);
    }

    /// 验证同一个顶层 ZIP 中的 Runtime 条目会走批量读取路径并保持输入顺序。
    #[test]
    fn batches_top_level_zip_runtime_entries() {
        let dir = runtime_test_dir("zip-batch");
        let zip_path = dir.join("runtime.zip");
        let first_entry = "runtime/100&u&_api_zip_first&1782368843095&0&0.log";
        let second_entry = "runtime/200&u&_api_zip_second&1782368843096&0&0.log";
        let file = fs::File::create(&zip_path).expect("应能创建 ZIP 测试文件");
        let mut writer = ZipWriter::new(file);
        writer
            .start_file(first_entry, SimpleFileOptions::default())
            .expect("应能写入第一条 ZIP 日志");
        writer
            .write_all(b"1ms 0ms 0ms 0ms 0ms select first")
            .expect("应能写入第一条 ZIP 内容");
        writer
            .start_file(second_entry, SimpleFileOptions::default())
            .expect("应能写入第二条 ZIP 日志");
        writer
            .write_all(b"2ms 0ms 0ms 0ms 0ms select second")
            .expect("应能写入第二条 ZIP 内容");
        writer.finish().expect("应能完成 ZIP 测试文件");

        let targets = [second_entry, first_entry]
            .into_iter()
            .enumerate()
            .map(|(index, entry_path)| RuntimeAnalysisTarget {
                source_id: SourceId(index),
                location: SourceLocation::ArchiveEntry {
                    archive_path: zip_path.clone(),
                    root_format: ArchiveFormat::Zip,
                    container_entries: Vec::new(),
                    entry_path: entry_path.to_string(),
                    format: ArchiveFormat::Zip,
                    archive_depth: 0,
                },
                archive_probe_node: None,
                label: Path::new(entry_path)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
                path: format!("{}!/{entry_path}", zip_path.display()),
                kind: RuntimeAnalysisTargetKind::File,
                archive_passwords: ArchivePasswordStore::default(),
            })
            .collect::<Vec<_>>();

        let result = analyze_runtime_targets(targets, "UTF-8".to_string(), LoaderConfig::default());

        assert_eq!(result.requests.len(), 2);
        assert_eq!(result.requests[0].request_path, "/api/zip/second");
        assert_eq!(result.requests[0].sql_records[0].sql_text, "select second");
        assert_eq!(result.requests[1].request_path, "/api/zip/first");
        assert_eq!(result.requests[1].sql_records[0].sql_text, "select first");
        let _ = fs::remove_dir_all(&dir);
    }

    /// 验证大量顶层 ZIP Runtime 条目会保持顺序并完整解析。
    #[test]
    fn batches_many_top_level_zip_runtime_entries() {
        let dir = runtime_test_dir("zip-many-batch");
        let zip_path = dir.join("runtime_many.zip");
        let file = fs::File::create(&zip_path).expect("应能创建 ZIP 测试文件");
        let mut writer = ZipWriter::new(file);
        let mut entries = Vec::new();
        for index in 0..70 {
            let entry = format!(
                "runtime/{}&u&_api_zip_many_{index}&1782368843{:03}&0&0.log",
                100 + index,
                index
            );
            writer
                .start_file(&entry, SimpleFileOptions::default())
                .expect("应能写入 ZIP Runtime 日志");
            writer
                .write_all(format!("{}ms 0ms 0ms 0ms 0ms select {index}", index + 1).as_bytes())
                .expect("应能写入 ZIP Runtime 内容");
            entries.push(entry);
        }
        writer.finish().expect("应能完成 ZIP 测试文件");

        let targets = entries
            .iter()
            .rev()
            .enumerate()
            .map(|(index, entry_path)| RuntimeAnalysisTarget {
                source_id: SourceId(index),
                location: SourceLocation::ArchiveEntry {
                    archive_path: zip_path.clone(),
                    root_format: ArchiveFormat::Zip,
                    container_entries: Vec::new(),
                    entry_path: entry_path.clone(),
                    format: ArchiveFormat::Zip,
                    archive_depth: 0,
                },
                archive_probe_node: None,
                label: Path::new(entry_path)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
                path: format!("{}!/{entry_path}", zip_path.display()),
                kind: RuntimeAnalysisTargetKind::File,
                archive_passwords: ArchivePasswordStore::default(),
            })
            .collect::<Vec<_>>();

        let result = analyze_runtime_targets(targets, "UTF-8".to_string(), LoaderConfig::default());

        assert_eq!(result.requests.len(), 70);
        assert_eq!(result.requests[0].request_path, "/api/zip/many/69");
        assert_eq!(result.requests[0].sql_records[0].sql_text, "select 69");
        assert_eq!(result.requests[69].request_path, "/api/zip/many/0");
        assert_eq!(result.requests[69].sql_records[0].sql_text, "select 0");
        let _ = fs::remove_dir_all(&dir);
    }
}
