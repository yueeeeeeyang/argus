//! 文件职责：执行真实日志关键字搜索。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：按日志来源逐文件读取、逐行匹配关键字，并以批次形式回报全部搜索结果。

use std::ops::Range;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use regex::{Regex, RegexBuilder};

use crate::loader::{SourceId, SourceLocation};
use crate::reader::log_file_reader::{
    DisplayedLogLine, LogDocument, LogFileReader, LogReaderHandle, OpenLogRequest, PagedLogDocument,
};

/// 搜索结果回传批次大小；较大批次减少 UI 刷新次数，结果总量仍不截断。
const SEARCH_RESULT_BATCH_SIZE: usize = 1024;
/// 单次从日志文档读取的行数；降低高频进度通知对日志滚动的干扰。
const SEARCH_LINE_CHUNK_SIZE: usize = 4096;

/// 搜索范围，决定 UI 展示的进度类型和目标收集方式。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchScope {
    /// 当前激活日志文件。
    CurrentFile,
    /// 来源树中的一个目录或压缩包目录。
    Directory,
    /// 来源树中用户多选的日志文件。
    SelectedFiles,
}

impl SearchScope {
    /// 返回搜索范围中文名称，用于窗口分段控件和结果面板展示。
    pub fn label(self) -> &'static str {
        match self {
            Self::CurrentFile => "当前文件",
            Self::Directory => "目录",
            Self::SelectedFiles => "选中文件",
        }
    }
}

/// 一次搜索的关键字和基础选项。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchQuery {
    /// 原始关键字；空关键字由应用层拦截，不进入搜索引擎。
    pub keyword: String,
    /// 是否大小写敏感；当前 UI 默认 false。
    pub case_sensitive: bool,
    /// 是否按 Rust regex 语法解释关键字。
    pub regex_enabled: bool,
}

impl SearchQuery {
    /// 构造默认大小写不敏感的关键字查询。
    pub fn new(keyword: String) -> Self {
        Self {
            keyword,
            case_sensitive: false,
            regex_enabled: false,
        }
    }
}

/// 单个可搜索日志目标。
#[derive(Clone, Debug)]
pub struct SearchTarget {
    /// 来源树节点 ID，用于结果点击后打开对应日志 tab。
    pub source_id: SourceId,
    /// UI 展示名称。
    pub label: String,
    /// UI 展示路径。
    pub path: String,
    /// 真实读取位置，复用现有日志读取器处理本地文件与压缩包条目。
    pub location: SourceLocation,
}

/// 搜索任务请求。
#[derive(Clone, Debug)]
pub struct SearchRequest {
    /// 搜索 generation，用于 UI 丢弃过期任务事件。
    pub generation: usize,
    /// 搜索范围。
    pub scope: SearchScope,
    /// 搜索关键字。
    pub query: SearchQuery,
    /// 本次搜索目标。
    pub targets: Vec<SearchTarget>,
    /// 用户设置的默认编码名称。
    pub default_encoding: String,
}

/// 搜索进度；当前文件搜索看行进度，目录和选中文件搜索看文件进度。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchProgress {
    /// 已扫描文件数量。
    pub scanned_files: usize,
    /// 总文件数量。
    pub total_files: usize,
    /// 当前文件已扫描行数。
    pub scanned_lines: usize,
    /// 当前文件总行数。
    pub total_lines: usize,
    /// 当前正在扫描的文件路径。
    pub current_path: Option<String>,
}

/// 单条搜索结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchResult {
    /// 匹配所在来源节点。
    pub source_id: SourceId,
    /// 匹配文件展示名。
    pub label: String,
    /// 匹配文件展示路径。
    pub path: String,
    /// 0 基行号。
    pub line_number: usize,
    /// 行文本，不包含换行符。
    pub line_text: String,
    /// 关键字在行文本中的字节范围；范围基于 UTF-8 边界。
    pub match_ranges: Vec<Range<usize>>,
}

/// 搜索结束摘要。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchTaskSummary {
    /// 是否因为用户取消而结束。
    pub was_cancelled: bool,
    /// 已扫描文件数量。
    pub scanned_files: usize,
    /// 已扫描总行数。
    pub scanned_lines: usize,
    /// 匹配结果总数。
    pub matched_results: usize,
    /// 单文件读取失败等非致命错误。
    pub errors: Vec<String>,
}

/// 当前日志快速查找扫描结果。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CurrentLogMatchScan {
    /// 按行保存的命中结果；单行内可能包含多个命中范围。
    pub matches: Vec<SearchResult>,
    /// 关键字在当前日志中出现的总次数。
    pub match_count: usize,
    /// 已扫描行数，便于测试确认没有整文件字符串化。
    pub scanned_lines: usize,
}

/// 当前日志快速计数结果；只保存出现次数，不缓存所有命中行。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CurrentLogMatchCount {
    /// 关键字在当前日志中出现的总次数。
    pub match_count: usize,
    /// 已扫描行数。
    pub scanned_lines: usize,
}

/// 当前日志快速定位方向；用于“上一个/下一个”只查找最近命中，避免整文件计数。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CurrentLogMatchDirection {
    /// 从当前位置向后查找。
    Next,
    /// 从当前位置向前查找。
    Previous,
}

/// 当前日志快速定位的起始位置。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CurrentLogMatchPosition {
    /// 0 基行号。
    pub line_number: usize,
    /// 当前行内已激活的命中序号；为空时表示从该行正常开始查找。
    pub match_index: Option<usize>,
}

/// 当前日志快速定位结果。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CurrentLogMatchNavigation {
    /// 找到的命中行；没有匹配时为空。
    pub result: Option<SearchResult>,
    /// 当前要强调的单个命中范围。
    pub active_range: Option<Range<usize>>,
    /// 当前命中在文件中的行号和行内序号。
    pub position: Option<CurrentLogMatchPosition>,
    /// 本次定位实际扫描的行数；用于测试和性能回归判断。
    pub scanned_lines: usize,
}

/// 真实搜索引擎；保持无状态，便于单元测试和后台任务复用。
#[derive(Debug, Default)]
pub struct SearchEngine;

impl SearchEngine {
    /// 校验搜索查询；正则模式会提前编译表达式并返回用户可读错误。
    pub fn validate_query(query: &SearchQuery) -> Result<(), String> {
        SearchMatcher::new(query).map(|_| ())
    }

    /// 扫描单个已打开日志句柄，返回当前文件中的全部命中。
    ///
    /// 参数说明：
    /// - `target`：当前日志在来源树中的展示信息。
    /// - `handle`：已打开日志读取句柄，内部会按行块读取。
    /// - `query`：搜索关键字、大小写和正则选项。
    /// - `cancel_token`：外部取消标记，旧任务会在行块边界尽快退出。
    ///
    /// 返回值：当前日志所有命中行与出现总次数；不会读取目录或其它日志文件。
    pub fn scan_current_log_matches(
        target: SearchTarget,
        handle: LogReaderHandle,
        query: SearchQuery,
        cancel_token: Arc<AtomicBool>,
    ) -> Result<CurrentLogMatchScan, String> {
        let matcher = SearchMatcher::new(&query)?;
        let mut scan = CurrentLogMatchScan::default();
        let line_count = handle.line_count();
        let mut start_line = 0;

        while start_line < line_count {
            if cancel_token.load(Ordering::Relaxed) {
                break;
            }

            let max_lines = (line_count - start_line).min(SEARCH_LINE_CHUNK_SIZE);
            let lines = handle.lines(start_line, max_lines).map_err(|error| {
                format!(
                    "{} 第 {} 行附近读取失败：{error}",
                    target.path,
                    start_line + 1
                )
            })?;

            for line in lines {
                if let Some(match_ranges) = matcher.find_match_ranges(&line.text) {
                    scan.match_count += match_ranges.len();
                    scan.matches.push(SearchResult {
                        source_id: target.source_id,
                        label: target.label.clone(),
                        path: target.path.clone(),
                        line_number: line.line_number,
                        line_text: line.text,
                        match_ranges,
                    });
                }
            }

            start_line += max_lines;
            scan.scanned_lines = start_line.min(line_count);
        }

        Ok(scan)
    }

    /// 统计单个已打开日志句柄中的关键字出现次数，不保存命中行。
    ///
    /// 参数说明：
    /// - `handle`：已打开日志读取句柄，分页日志会走按字节窗口合并读取的快速路径。
    /// - `query`：搜索关键字、大小写和正则选项。
    /// - `cancel_token`：外部取消标记，旧任务会在行边界尽快退出。
    ///
    /// 返回值：出现次数和扫描行数；该接口专供“计数”按钮使用，避免高频关键字占用大量内存。
    pub fn count_current_log_matches(
        handle: LogReaderHandle,
        query: SearchQuery,
        cancel_token: Arc<AtomicBool>,
    ) -> Result<CurrentLogMatchCount, String> {
        let matcher = SearchMatcher::new(&query)?;
        let mut count = CurrentLogMatchCount::default();

        match handle.document() {
            LogDocument::InMemory(document) => {
                for line in document.lines.iter() {
                    if cancel_token.load(Ordering::Relaxed) {
                        break;
                    }
                    count.match_count = count
                        .match_count
                        .saturating_add(matcher.count_in_line(line));
                    count.scanned_lines += 1;
                }
            }
            LogDocument::Paged(document) => {
                document
                    .for_each_line_in_range(0..document.line_count(), |line| {
                        if cancel_token.load(Ordering::Relaxed) {
                            return false;
                        }
                        count.match_count = count
                            .match_count
                            .saturating_add(matcher.count_in_line(&line.text));
                        count.scanned_lines += 1;
                        true
                    })
                    .map_err(|error| format!("当前日志计数失败：{error}"))?;
            }
        }

        Ok(count)
    }

    /// 从当前日志中的一个位置出发，只查找最近的上一个或下一个命中。
    ///
    /// 参数说明：
    /// - `target`：当前日志在来源树中的展示信息。
    /// - `handle`：已打开日志读取句柄。
    /// - `query`：搜索关键字、大小写和正则选项。
    /// - `start_position`：当前行和当前行内命中序号，用于避免重复定位同一个命中。
    /// - `direction`：向前或向后查找。
    /// - `cancel_token`：外部取消标记。
    ///
    /// 返回值：最近的单个命中；没有匹配时返回空结果。该接口不会构建完整计数缓存。
    pub fn find_current_log_match(
        target: SearchTarget,
        handle: LogReaderHandle,
        query: SearchQuery,
        start_position: CurrentLogMatchPosition,
        direction: CurrentLogMatchDirection,
        cancel_token: Arc<AtomicBool>,
    ) -> Result<CurrentLogMatchNavigation, String> {
        let matcher = SearchMatcher::new(&query)?;
        let line_count = handle.line_count();
        if line_count == 0 {
            return Ok(CurrentLogMatchNavigation::default());
        }

        let start_line = start_position.line_number.min(line_count - 1);
        let mut navigation = CurrentLogMatchNavigation::default();

        if let LogDocument::Paged(document) = handle.document() {
            find_current_log_match_in_paged(
                &target,
                document,
                &matcher,
                CurrentLogMatchPosition {
                    line_number: start_line,
                    match_index: start_position.match_index,
                },
                direction,
                &cancel_token,
                &mut navigation,
            )?;
            return Ok(navigation);
        }

        match direction {
            CurrentLogMatchDirection::Next => find_next_current_log_match(
                target,
                handle,
                &matcher,
                start_line,
                start_position.match_index,
                cancel_token,
                &mut navigation,
            )?,
            CurrentLogMatchDirection::Previous => find_previous_current_log_match(
                target,
                handle,
                &matcher,
                start_line,
                start_position.match_index,
                cancel_token,
                &mut navigation,
            )?,
        }

        Ok(navigation)
    }

    /// 执行搜索并通过回调持续报告进度和结果批次。
    ///
    /// 参数说明：
    /// - `request`：搜索范围、关键字和目标列表。
    /// - `progress_callback`：每扫描一个行块或切换文件时调用。
    /// - `result_batch_callback`：每积累一批匹配结果时调用，任务结束前会 flush 剩余结果。
    /// - `cancel_token`：外部取消标记，搜索循环会在文件和行块边界检查。
    ///
    /// 返回值：搜索结束摘要；单文件失败会写入 errors，不中断其他文件。
    pub fn search(
        request: SearchRequest,
        mut progress_callback: impl FnMut(SearchProgress),
        mut result_batch_callback: impl FnMut(Vec<SearchResult>),
        cancel_token: Arc<AtomicBool>,
    ) -> SearchTaskSummary {
        let mut summary = SearchTaskSummary::default();
        let matcher = match SearchMatcher::new(&request.query) {
            Ok(matcher) => matcher,
            Err(message) => {
                summary.errors.push(message);
                return summary;
            }
        };
        let mut result_batch = Vec::with_capacity(SEARCH_RESULT_BATCH_SIZE);
        let mut progress = SearchProgress {
            total_files: request.targets.len(),
            ..SearchProgress::default()
        };

        progress_callback(progress.clone());

        for target in request.targets {
            if cancel_token.load(Ordering::Relaxed) {
                summary.was_cancelled = true;
                break;
            }

            progress.current_path = Some(target.path.clone());
            progress.scanned_lines = 0;
            progress.total_lines = 0;
            progress_callback(progress.clone());

            let handle = match LogFileReader::open(OpenLogRequest {
                source_id: target.source_id,
                location: target.location.clone(),
                label: target.label.clone(),
                default_encoding: request.default_encoding.clone(),
            }) {
                Ok(handle) => handle,
                Err(error) => {
                    summary.scanned_files += 1;
                    progress.scanned_files += 1;
                    summary
                        .errors
                        .push(format!("{} 搜索前读取失败：{error}", target.path));
                    progress_callback(progress.clone());
                    continue;
                }
            };

            progress.total_lines = handle.line_count();
            progress_callback(progress.clone());

            let mut start_line = 0;
            while start_line < handle.line_count() {
                if cancel_token.load(Ordering::Relaxed) {
                    summary.was_cancelled = true;
                    break;
                }

                let max_lines = (handle.line_count() - start_line).min(SEARCH_LINE_CHUNK_SIZE);
                match handle.lines(start_line, max_lines) {
                    Ok(lines) => {
                        for line in lines {
                            if let Some(match_ranges) = matcher.find_match_ranges(&line.text) {
                                result_batch.push(SearchResult {
                                    source_id: target.source_id,
                                    label: target.label.clone(),
                                    path: target.path.clone(),
                                    line_number: line.line_number,
                                    line_text: line.text,
                                    match_ranges,
                                });
                                summary.matched_results += 1;

                                if result_batch.len() >= SEARCH_RESULT_BATCH_SIZE {
                                    result_batch_callback(std::mem::take(&mut result_batch));
                                }
                            }
                        }
                    }
                    Err(error) => {
                        summary.errors.push(format!(
                            "{} 第 {} 行附近读取失败：{error}",
                            target.path,
                            start_line + 1
                        ));
                        break;
                    }
                }

                start_line += max_lines;
                progress.scanned_lines = start_line.min(handle.line_count());
                summary.scanned_lines += max_lines;
                progress_callback(progress.clone());
            }

            if summary.was_cancelled {
                break;
            }

            summary.scanned_files += 1;
            progress.scanned_files += 1;
            progress_callback(progress.clone());
        }

        if !result_batch.is_empty() {
            result_batch_callback(result_batch);
        }

        summary
    }
}

/// 向后查找最近命中；分两段扫描以支持到末尾后循环到文件开头。
fn find_next_current_log_match(
    target: SearchTarget,
    handle: LogReaderHandle,
    matcher: &SearchMatcher,
    start_line: usize,
    start_match_index: Option<usize>,
    cancel_token: Arc<AtomicBool>,
    navigation: &mut CurrentLogMatchNavigation,
) -> Result<(), String> {
    let line_count = handle.line_count();
    if find_forward_inclusive_segment(
        &target,
        &handle,
        matcher,
        start_line,
        line_count,
        start_match_index.map(|index| index + 1),
        &cancel_token,
        navigation,
    )? {
        return Ok(());
    }

    if start_line > 0
        && find_forward_inclusive_segment(
            &target,
            &handle,
            matcher,
            0,
            start_line,
            None,
            &cancel_token,
            navigation,
        )?
    {
        return Ok(());
    }

    // 只有一个命中时，循环导航应允许回到当前命中。
    if let Some(current_match_index) = start_match_index {
        find_forward_single_line_prefix(
            &target,
            &handle,
            matcher,
            start_line,
            current_match_index + 1,
            &cancel_token,
            navigation,
        )?;
    }

    Ok(())
}

/// 向前查找最近命中；分两段扫描以支持到开头后循环到文件末尾。
fn find_previous_current_log_match(
    target: SearchTarget,
    handle: LogReaderHandle,
    matcher: &SearchMatcher,
    start_line: usize,
    start_match_index: Option<usize>,
    cancel_token: Arc<AtomicBool>,
    navigation: &mut CurrentLogMatchNavigation,
) -> Result<(), String> {
    if find_backward_inclusive_segment(
        &target,
        &handle,
        matcher,
        0,
        start_line + 1,
        start_match_index,
        &cancel_token,
        navigation,
    )? {
        return Ok(());
    }

    let line_count = handle.line_count();
    if start_line + 1 < line_count
        && find_backward_inclusive_segment(
            &target,
            &handle,
            matcher,
            start_line + 1,
            line_count,
            None,
            &cancel_token,
            navigation,
        )?
    {
        return Ok(());
    }

    // 只有一个命中时，循环导航应允许回到当前命中。
    if let Some(current_match_index) = start_match_index {
        find_backward_single_line_suffix(
            &target,
            &handle,
            matcher,
            start_line,
            current_match_index,
            &cancel_token,
            navigation,
        )?;
    }

    Ok(())
}

/// 在分页日志中执行上/下一个定位，读取时按字节窗口顺序扫描，找到最近命中立即停止。
fn find_current_log_match_in_paged(
    target: &SearchTarget,
    document: &PagedLogDocument,
    matcher: &SearchMatcher,
    start_position: CurrentLogMatchPosition,
    direction: CurrentLogMatchDirection,
    cancel_token: &Arc<AtomicBool>,
    navigation: &mut CurrentLogMatchNavigation,
) -> Result<(), String> {
    match direction {
        CurrentLogMatchDirection::Next => find_next_current_log_match_in_paged(
            target,
            document,
            matcher,
            start_position,
            cancel_token,
            navigation,
        ),
        CurrentLogMatchDirection::Previous => find_previous_current_log_match_in_paged(
            target,
            document,
            matcher,
            start_position,
            cancel_token,
            navigation,
        ),
    }
}

/// 在分页日志中向后查找最近命中；先检查当前行剩余命中，再扫描后续范围，最后循环到开头。
fn find_next_current_log_match_in_paged(
    target: &SearchTarget,
    document: &PagedLogDocument,
    matcher: &SearchMatcher,
    start_position: CurrentLogMatchPosition,
    cancel_token: &Arc<AtomicBool>,
    navigation: &mut CurrentLogMatchNavigation,
) -> Result<(), String> {
    let line_count = document.line_count();
    let start_line = start_position.line_number.min(line_count.saturating_sub(1));

    if let Some(current_match_index) = start_position.match_index {
        if find_paged_single_line_from_index(
            target,
            document,
            matcher,
            start_line,
            current_match_index + 1,
            cancel_token,
            navigation,
        )? {
            return Ok(());
        }

        if start_line + 1 < line_count
            && find_forward_paged_segment(
                target,
                document,
                matcher,
                start_line + 1,
                line_count,
                None,
                cancel_token,
                navigation,
            )?
        {
            return Ok(());
        }
    } else if find_forward_paged_segment(
        target,
        document,
        matcher,
        start_line,
        line_count,
        None,
        cancel_token,
        navigation,
    )? {
        return Ok(());
    }

    if start_line > 0
        && find_forward_paged_segment(
            target,
            document,
            matcher,
            0,
            start_line,
            None,
            cancel_token,
            navigation,
        )?
    {
        return Ok(());
    }

    // 只有一个命中时，循环导航允许回到当前行当前命中及其之前的范围。
    if start_position.match_index.is_some() {
        find_paged_single_line_from_index(
            target,
            document,
            matcher,
            start_line,
            0,
            cancel_token,
            navigation,
        )?;
    }

    Ok(())
}

/// 在分页日志中向前查找最近命中；先检查当前行前序命中，再扫描前方范围，最后循环到末尾。
fn find_previous_current_log_match_in_paged(
    target: &SearchTarget,
    document: &PagedLogDocument,
    matcher: &SearchMatcher,
    start_position: CurrentLogMatchPosition,
    cancel_token: &Arc<AtomicBool>,
    navigation: &mut CurrentLogMatchNavigation,
) -> Result<(), String> {
    let line_count = document.line_count();
    let start_line = start_position.line_number.min(line_count.saturating_sub(1));

    if let Some(current_match_index) = start_position.match_index {
        if find_paged_single_line_before_index(
            target,
            document,
            matcher,
            start_line,
            current_match_index,
            cancel_token,
            navigation,
        )? {
            return Ok(());
        }

        if start_line > 0
            && find_backward_paged_segment(
                target,
                document,
                matcher,
                0,
                start_line,
                None,
                cancel_token,
                navigation,
            )?
        {
            return Ok(());
        }
    } else if find_backward_paged_segment(
        target,
        document,
        matcher,
        0,
        start_line + 1,
        None,
        cancel_token,
        navigation,
    )? {
        return Ok(());
    }

    if start_line + 1 < line_count
        && find_backward_paged_segment(
            target,
            document,
            matcher,
            start_line + 1,
            line_count,
            None,
            cancel_token,
            navigation,
        )?
    {
        return Ok(());
    }

    // 只有一个命中时，循环导航允许回到当前行当前命中及其之后的范围。
    if start_position.match_index.is_some() {
        find_paged_single_line_before_index(
            target,
            document,
            matcher,
            start_line,
            usize::MAX,
            cancel_token,
            navigation,
        )?;
    }

    Ok(())
}

/// 正向扫描分页日志半开区间 `[start_line, end_line)`，找到首个命中后立即停止。
fn find_forward_paged_segment(
    target: &SearchTarget,
    document: &PagedLogDocument,
    matcher: &SearchMatcher,
    start_line: usize,
    end_line: usize,
    first_line_min_match_index: Option<usize>,
    cancel_token: &Arc<AtomicBool>,
    navigation: &mut CurrentLogMatchNavigation,
) -> Result<bool, String> {
    let mut found = false;
    document
        .for_each_line_in_range(start_line..end_line, |line| {
            if cancel_token.load(Ordering::Relaxed) {
                return false;
            }
            navigation.scanned_lines += 1;
            let Some(match_ranges) = matcher.find_match_ranges(&line.text) else {
                return true;
            };
            let min_index = if line.line_number == start_line {
                first_line_min_match_index.unwrap_or(0)
            } else {
                0
            };
            let Some((match_index, active_range)) =
                first_match_from_index(&match_ranges, min_index)
            else {
                return true;
            };
            write_navigation_result(
                target,
                navigation,
                line,
                match_ranges,
                match_index,
                active_range,
            );
            found = true;
            false
        })
        .map_err(|error| {
            format!(
                "{} 第 {} 行附近读取失败：{error}",
                target.path,
                start_line + 1
            )
        })?;

    Ok(found)
}

/// 反向扫描分页日志半开区间 `[start_line, end_line)`，找到首个命中后立即停止。
fn find_backward_paged_segment(
    target: &SearchTarget,
    document: &PagedLogDocument,
    matcher: &SearchMatcher,
    start_line: usize,
    end_line: usize,
    first_line_max_match_index: Option<usize>,
    cancel_token: &Arc<AtomicBool>,
    navigation: &mut CurrentLogMatchNavigation,
) -> Result<bool, String> {
    let mut found = false;
    document
        .for_each_line_in_range_rev(start_line..end_line, |line| {
            if cancel_token.load(Ordering::Relaxed) {
                return false;
            }
            navigation.scanned_lines += 1;
            let Some(match_ranges) = matcher.find_match_ranges(&line.text) else {
                return true;
            };
            let max_index = if line.line_number + 1 == end_line {
                first_line_max_match_index.unwrap_or(match_ranges.len())
            } else {
                match_ranges.len()
            };
            let Some((match_index, active_range)) =
                last_match_before_index(&match_ranges, max_index)
            else {
                return true;
            };
            write_navigation_result(
                target,
                navigation,
                line,
                match_ranges,
                match_index,
                active_range,
            );
            found = true;
            false
        })
        .map_err(|error| {
            format!(
                "{} 第 {} 行附近读取失败：{error}",
                target.path,
                start_line + 1
            )
        })?;

    Ok(found)
}

/// 在分页日志单行中查找指定序号及之后的第一个命中。
fn find_paged_single_line_from_index(
    target: &SearchTarget,
    document: &PagedLogDocument,
    matcher: &SearchMatcher,
    line_number: usize,
    min_match_index: usize,
    cancel_token: &Arc<AtomicBool>,
    navigation: &mut CurrentLogMatchNavigation,
) -> Result<bool, String> {
    if cancel_token.load(Ordering::Relaxed) {
        return Ok(false);
    }
    let Some(line) = read_paged_displayed_line(target, document, line_number)? else {
        return Ok(false);
    };
    navigation.scanned_lines += 1;
    let Some(match_ranges) = matcher.find_match_ranges(&line.text) else {
        return Ok(false);
    };
    let Some((match_index, active_range)) = first_match_from_index(&match_ranges, min_match_index)
    else {
        return Ok(false);
    };
    write_navigation_result(
        target,
        navigation,
        line,
        match_ranges,
        match_index,
        active_range,
    );
    Ok(true)
}

/// 在分页日志单行中查找指定序号之前的最后一个命中。
fn find_paged_single_line_before_index(
    target: &SearchTarget,
    document: &PagedLogDocument,
    matcher: &SearchMatcher,
    line_number: usize,
    max_match_index: usize,
    cancel_token: &Arc<AtomicBool>,
    navigation: &mut CurrentLogMatchNavigation,
) -> Result<bool, String> {
    if cancel_token.load(Ordering::Relaxed) {
        return Ok(false);
    }
    let Some(line) = read_paged_displayed_line(target, document, line_number)? else {
        return Ok(false);
    };
    navigation.scanned_lines += 1;
    let Some(match_ranges) = matcher.find_match_ranges(&line.text) else {
        return Ok(false);
    };
    let Some((match_index, active_range)) = last_match_before_index(&match_ranges, max_match_index)
    else {
        return Ok(false);
    };
    write_navigation_result(
        target,
        navigation,
        line,
        match_ranges,
        match_index,
        active_range,
    );
    Ok(true)
}

/// 读取分页日志单行并映射为搜索展示行。
fn read_paged_displayed_line(
    target: &SearchTarget,
    document: &PagedLogDocument,
    line_number: usize,
) -> Result<Option<DisplayedLogLine>, String> {
    document
        .read_line(line_number)
        .map(|line| {
            line.map(|line| DisplayedLogLine {
                line_number: line.line_number,
                text: line.text.to_string(),
            })
        })
        .map_err(|error| {
            format!(
                "{} 第 {} 行附近读取失败：{error}",
                target.path,
                line_number + 1
            )
        })
}

/// 正向扫描半开区间 `[start_line, end_line)`，在找到第一个命中后立即停止。
fn find_forward_inclusive_segment(
    target: &SearchTarget,
    handle: &LogReaderHandle,
    matcher: &SearchMatcher,
    start_line: usize,
    end_line: usize,
    first_line_min_match_index: Option<usize>,
    cancel_token: &Arc<AtomicBool>,
    navigation: &mut CurrentLogMatchNavigation,
) -> Result<bool, String> {
    let mut current_line = start_line;
    while current_line < end_line {
        if cancel_token.load(Ordering::Relaxed) {
            return Ok(false);
        }

        let max_lines = (end_line - current_line).min(SEARCH_LINE_CHUNK_SIZE);
        let lines = handle.lines(current_line, max_lines).map_err(|error| {
            format!(
                "{} 第 {} 行附近读取失败：{error}",
                target.path,
                current_line + 1
            )
        })?;

        for line in lines {
            navigation.scanned_lines += 1;
            if let Some(match_ranges) = matcher.find_match_ranges(&line.text) {
                let min_index = if line.line_number == start_line {
                    first_line_min_match_index.unwrap_or(0)
                } else {
                    0
                };
                if let Some((match_index, active_range)) =
                    first_match_from_index(&match_ranges, min_index)
                {
                    write_navigation_result(
                        target,
                        navigation,
                        line,
                        match_ranges,
                        match_index,
                        active_range,
                    );
                    return Ok(true);
                }
            }
        }

        current_line += max_lines;
    }

    Ok(false)
}

/// 反向扫描半开区间 `[start_line, end_line)`，在找到第一个命中后立即停止。
fn find_backward_inclusive_segment(
    target: &SearchTarget,
    handle: &LogReaderHandle,
    matcher: &SearchMatcher,
    start_line: usize,
    end_line: usize,
    first_line_max_match_index: Option<usize>,
    cancel_token: &Arc<AtomicBool>,
    navigation: &mut CurrentLogMatchNavigation,
) -> Result<bool, String> {
    let mut current_end = end_line;
    while current_end > start_line {
        if cancel_token.load(Ordering::Relaxed) {
            return Ok(false);
        }

        let chunk_start = current_end
            .saturating_sub(SEARCH_LINE_CHUNK_SIZE)
            .max(start_line);
        let lines = handle
            .lines(chunk_start, current_end - chunk_start)
            .map_err(|error| {
                format!(
                    "{} 第 {} 行附近读取失败：{error}",
                    target.path,
                    chunk_start + 1
                )
            })?;

        for line in lines.into_iter().rev() {
            navigation.scanned_lines += 1;
            if let Some(match_ranges) = matcher.find_match_ranges(&line.text) {
                let max_index = if line.line_number + 1 == end_line {
                    first_line_max_match_index.unwrap_or(match_ranges.len())
                } else {
                    match_ranges.len()
                };
                if let Some((match_index, active_range)) =
                    last_match_before_index(&match_ranges, max_index)
                {
                    write_navigation_result(
                        target,
                        navigation,
                        line,
                        match_ranges,
                        match_index,
                        active_range,
                    );
                    return Ok(true);
                }
            }
        }

        current_end = chunk_start;
    }

    Ok(false)
}

/// 正向循环回到起始行时，只扫描当前命中及其之前的范围。
fn find_forward_single_line_prefix(
    target: &SearchTarget,
    handle: &LogReaderHandle,
    matcher: &SearchMatcher,
    line_number: usize,
    max_match_count: usize,
    cancel_token: &Arc<AtomicBool>,
    navigation: &mut CurrentLogMatchNavigation,
) -> Result<bool, String> {
    if cancel_token.load(Ordering::Relaxed) {
        return Ok(false);
    }
    let mut lines = handle.lines(line_number, 1).map_err(|error| {
        format!(
            "{} 第 {} 行附近读取失败：{error}",
            target.path,
            line_number + 1
        )
    })?;
    let Some(line) = lines.pop() else {
        return Ok(false);
    };
    navigation.scanned_lines += 1;
    let Some(match_ranges) = matcher.find_match_ranges(&line.text) else {
        return Ok(false);
    };
    let prefix_count = max_match_count.min(match_ranges.len());
    if prefix_count == 0 {
        return Ok(false);
    }
    let active_range = match_ranges[0].clone();
    write_navigation_result(target, navigation, line, match_ranges, 0, active_range);
    Ok(true)
}

/// 反向循环回到起始行时，只扫描当前命中及其之后的范围。
fn find_backward_single_line_suffix(
    target: &SearchTarget,
    handle: &LogReaderHandle,
    matcher: &SearchMatcher,
    line_number: usize,
    min_match_index: usize,
    cancel_token: &Arc<AtomicBool>,
    navigation: &mut CurrentLogMatchNavigation,
) -> Result<bool, String> {
    if cancel_token.load(Ordering::Relaxed) {
        return Ok(false);
    }
    let mut lines = handle.lines(line_number, 1).map_err(|error| {
        format!(
            "{} 第 {} 行附近读取失败：{error}",
            target.path,
            line_number + 1
        )
    })?;
    let Some(line) = lines.pop() else {
        return Ok(false);
    };
    navigation.scanned_lines += 1;
    let Some(match_ranges) = matcher.find_match_ranges(&line.text) else {
        return Ok(false);
    };
    if min_match_index >= match_ranges.len() {
        return Ok(false);
    }
    let match_index = match_ranges.len() - 1;
    let active_range = match_ranges[match_index].clone();
    write_navigation_result(
        target,
        navigation,
        line,
        match_ranges,
        match_index,
        active_range,
    );
    Ok(true)
}

/// 返回从指定序号开始的第一个命中范围。
fn first_match_from_index(
    match_ranges: &[Range<usize>],
    min_index: usize,
) -> Option<(usize, Range<usize>)> {
    match_ranges
        .iter()
        .enumerate()
        .skip(min_index)
        .map(|(index, range)| (index, range.clone()))
        .next()
}

/// 返回指定序号之前的最后一个命中范围。
fn last_match_before_index(
    match_ranges: &[Range<usize>],
    max_index: usize,
) -> Option<(usize, Range<usize>)> {
    let clamped_max = max_index.min(match_ranges.len());
    match_ranges
        .iter()
        .enumerate()
        .take(clamped_max)
        .last()
        .map(|(index, range)| (index, range.clone()))
}

/// 写入导航结果，保留整行所有命中范围，同时记录当前单个高亮范围。
fn write_navigation_result(
    target: &SearchTarget,
    navigation: &mut CurrentLogMatchNavigation,
    line: DisplayedLogLine,
    match_ranges: Vec<Range<usize>>,
    match_index: usize,
    active_range: Range<usize>,
) {
    navigation.result = Some(SearchResult {
        source_id: target.source_id,
        label: target.label.clone(),
        path: target.path.clone(),
        line_number: line.line_number,
        line_text: line.text,
        match_ranges,
    });
    navigation.active_range = Some(active_range);
    navigation.position = Some(CurrentLogMatchPosition {
        line_number: line.line_number,
        match_index: Some(match_index),
    });
}

/// 编译后的搜索匹配器，保证正则表达式只在任务启动时构造一次。
pub enum SearchMatcher {
    /// 普通关键字搜索。
    Literal(SearchQuery),
    /// 正则表达式搜索。
    Regex(Regex),
}

impl SearchMatcher {
    /// 根据查询构造匹配器，并把底层正则错误映射为中文提示。
    pub fn new(query: &SearchQuery) -> Result<Self, String> {
        if query.regex_enabled {
            compile_search_regex(query).map(Self::Regex)
        } else {
            Ok(Self::Literal(query.clone()))
        }
    }

    /// 在单行文本中查找所有命中范围。
    pub fn find_match_ranges(&self, line: &str) -> Option<Vec<Range<usize>>> {
        match self {
            Self::Literal(query) => find_literal_match_ranges(line, query),
            Self::Regex(regex) => find_regex_match_ranges(line, regex),
        }
    }

    /// 统计单行文本中的命中次数，不构造 `SearchResult` 或范围数组。
    ///
    /// 参数说明：
    /// - `line`：已经解码并展开 tab 的展示行文本。
    ///
    /// 返回值：该行中的非重叠命中次数；正则零宽命中会被跳过，避免无意义计数。
    pub fn count_in_line(&self, line: &str) -> usize {
        match self {
            Self::Literal(query) => count_literal_matches(line, query),
            Self::Regex(regex) => regex
                .find_iter(line)
                .filter(|match_item| match_item.start() != match_item.end())
                .count(),
        }
    }
}

/// 在单行文本中查找所有关键字命中范围。
#[cfg(test)]
fn find_match_ranges(line: &str, query: &SearchQuery) -> Option<Vec<Range<usize>>> {
    SearchMatcher::new(query)
        .ok()
        .and_then(|matcher| matcher.find_match_ranges(line))
}

/// 在单行文本中执行普通关键字搜索。
fn find_literal_match_ranges(line: &str, query: &SearchQuery) -> Option<Vec<Range<usize>>> {
    if query.keyword.is_empty() {
        return None;
    }

    let mut ranges = Vec::new();
    if query.case_sensitive {
        collect_match_ranges(line, &query.keyword, &mut ranges);
    } else {
        collect_case_insensitive_match_ranges(line, &query.keyword, &mut ranges);
    }

    if ranges.is_empty() {
        None
    } else {
        Some(ranges)
    }
}

/// 统计普通关键字在单行中的非重叠出现次数。
fn count_literal_matches(line: &str, query: &SearchQuery) -> usize {
    if query.keyword.is_empty() {
        return 0;
    }

    if query.case_sensitive {
        return count_non_overlapping_matches(line, &query.keyword);
    }

    count_case_insensitive_matches(line, &query.keyword)
}

/// 统计大小写不敏感命中次数，并在 ASCII 常见路径上避免逐字符归一化。
fn count_case_insensitive_matches(line: &str, keyword: &str) -> usize {
    if line.is_ascii() && keyword.is_ascii() {
        let normalized_line = line.to_ascii_lowercase();
        let normalized_keyword = keyword.to_ascii_lowercase();
        return count_non_overlapping_matches(&normalized_line, &normalized_keyword);
    }

    let normalized_keyword = keyword.to_lowercase();
    let characters = line.char_indices().collect::<Vec<_>>();
    let mut start_index = 0;
    let mut count = 0;

    while start_index < characters.len() {
        let mut normalized_candidate = String::new();
        let mut matched_end_index = None;

        for offset in 0..(characters.len() - start_index) {
            normalized_candidate.extend(characters[start_index + offset].1.to_lowercase());

            if !normalized_keyword.starts_with(&normalized_candidate) {
                break;
            }

            if normalized_candidate == normalized_keyword {
                matched_end_index = Some(start_index + offset + 1);
                break;
            }
        }

        if let Some(end_index) = matched_end_index {
            count += 1;
            start_index = end_index;
        } else {
            start_index += 1;
        }
    }

    count
}

/// 编译搜索正则表达式。
fn compile_search_regex(query: &SearchQuery) -> Result<Regex, String> {
    RegexBuilder::new(&query.keyword)
        .case_insensitive(!query.case_sensitive)
        .build()
        .map_err(|error| format!("正则表达式无效：{error}"))
}

/// 收集正则命中范围；跳过零宽命中，避免结果高亮和行匹配出现空范围。
fn find_regex_match_ranges(line: &str, regex: &Regex) -> Option<Vec<Range<usize>>> {
    let ranges = regex
        .find_iter(line)
        .filter_map(|match_item| {
            let range = match_item.start()..match_item.end();
            if range.start == range.end {
                None
            } else {
                Some(range)
            }
        })
        .collect::<Vec<_>>();

    if ranges.is_empty() {
        None
    } else {
        Some(ranges)
    }
}

/// 使用大小写不敏感策略收集命中范围，并保证范围始终指向原始行文本。
fn collect_case_insensitive_match_ranges(
    line: &str,
    keyword: &str,
    ranges: &mut Vec<Range<usize>>,
) {
    if line.is_ascii() && keyword.is_ascii() {
        let normalized_line = line.to_ascii_lowercase();
        let normalized_keyword = keyword.to_ascii_lowercase();
        collect_match_ranges(&normalized_line, &normalized_keyword, ranges);
        return;
    }

    let normalized_keyword = keyword.to_lowercase();
    let characters = line.char_indices().collect::<Vec<_>>();
    let mut start_index = 0;

    while start_index < characters.len() {
        let start_byte = characters[start_index].0;
        let mut normalized_candidate = String::new();
        let mut matched_end_index = None;

        for offset in 0..(characters.len() - start_index) {
            normalized_candidate.extend(characters[start_index + offset].1.to_lowercase());

            if !normalized_keyword.starts_with(&normalized_candidate) {
                break;
            }

            if normalized_candidate == normalized_keyword {
                matched_end_index = Some(start_index + offset + 1);
                break;
            }
        }

        if let Some(end_index) = matched_end_index {
            let end_byte = characters
                .get(end_index)
                .map(|(byte, _)| *byte)
                .unwrap_or(line.len());
            ranges.push(start_byte..end_byte);
            start_index = end_index;
        } else {
            start_index += 1;
        }
    }
}

/// 使用非重叠匹配策略收集关键字范围；空关键字由调用方拦截。
fn collect_match_ranges(haystack: &str, needle: &str, ranges: &mut Vec<Range<usize>>) {
    let mut search_from = 0;
    while let Some(relative) = haystack[search_from..].find(needle) {
        let start = search_from + relative;
        let end = start + needle.len();
        ranges.push(start..end);
        search_from = end;
    }
}

/// 统计非重叠字面量命中次数；空 needle 由调用方拦截。
fn count_non_overlapping_matches(haystack: &str, needle: &str) -> usize {
    let mut search_from = 0;
    let mut count = 0;
    while let Some(relative) = haystack[search_from..].find(needle) {
        let end = search_from + relative + needle.len();
        count += 1;
        search_from = end;
    }
    count
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{Arc, atomic::AtomicBool};

    use super::{
        CurrentLogMatchDirection, CurrentLogMatchPosition, SearchEngine, SearchQuery,
        SearchRequest, SearchScope, SearchTarget, find_match_ranges,
    };
    use crate::loader::{SourceId, SourceLocation};
    use crate::reader::log_file_reader::{LogFileReader, OpenLogRequest};

    /// 验证搜索默认大小写不敏感并返回全部命中范围。
    #[test]
    fn finds_all_case_insensitive_matches() {
        let query = SearchQuery::new("error".to_string());

        let ranges = find_match_ranges("ERROR warn error", &query).unwrap();

        assert_eq!(ranges, vec![0..5, 11..16]);
    }

    /// 验证 Unicode 大小写折叠后返回的范围仍然落在原始文本边界上。
    #[test]
    fn case_insensitive_unicode_ranges_point_to_original_line() {
        let line = "İ ERROR";
        let query = SearchQuery::new("error".to_string());

        let ranges = find_match_ranges(line, &query).unwrap();

        assert_eq!(ranges, vec![3..8]);
        assert_eq!(&line[ranges[0].clone()], "ERROR");
    }

    /// 验证大小写敏感搜索只返回精确大小写命中。
    #[test]
    fn respects_case_sensitive_query() {
        let query = SearchQuery {
            keyword: "error".to_string(),
            case_sensitive: true,
            regex_enabled: false,
        };

        let ranges = find_match_ranges("ERROR error", &query).unwrap();

        assert_eq!(ranges, vec![6..11]);
    }

    /// 验证正则搜索能返回每个命中的原始文本范围。
    #[test]
    fn regex_query_finds_all_regex_matches() {
        let query = SearchQuery {
            keyword: r"ERROR|WARN|\d{4}-\d{2}-\d{2}".to_string(),
            case_sensitive: true,
            regex_enabled: true,
        };

        let ranges = find_match_ranges("2026-06-11 INFO WARN ERROR", &query).unwrap();

        assert_eq!(ranges, vec![0..10, 16..20, 21..26]);
    }

    /// 验证正则搜索默认同样不区分大小写。
    #[test]
    fn regex_query_respects_case_insensitive_default() {
        let query = SearchQuery {
            keyword: "error".to_string(),
            case_sensitive: false,
            regex_enabled: true,
        };

        let ranges = find_match_ranges("Error ERROR", &query).unwrap();

        assert_eq!(ranges, vec![0..5, 6..11]);
    }

    /// 验证非法正则会被提前转换为用户可读错误。
    #[test]
    fn invalid_regex_query_is_reported_without_scanning_files() {
        let request = SearchRequest {
            generation: 1,
            scope: SearchScope::CurrentFile,
            query: SearchQuery {
                keyword: "(".to_string(),
                case_sensitive: false,
                regex_enabled: true,
            },
            targets: vec![SearchTarget {
                source_id: SourceId(1),
                label: "test.log".to_string(),
                path: "test.log".to_string(),
                location: SourceLocation::LocalPath(std::env::temp_dir().join("missing.log")),
            }],
            default_encoding: "UTF-8".to_string(),
        };

        let summary =
            SearchEngine::search(request, |_| {}, |_| {}, Arc::new(AtomicBool::new(false)));

        assert_eq!(summary.scanned_files, 0);
        assert_eq!(summary.errors.len(), 1);
        assert!(summary.errors[0].starts_with("正则表达式无效："));
    }

    /// 验证当前日志快速扫描按出现次数计数，而不是按命中行数计数。
    #[test]
    fn current_log_scan_counts_occurrences_not_lines() {
        let path = std::env::temp_dir().join(format!(
            "argus-current-count-test-{}-{}.log",
            std::process::id(),
            2
        ));
        fs::write(&path, "ERROR ERROR\ninfo\nerror\n").unwrap();
        let source_id = SourceId(7);
        let handle = LogFileReader::open(OpenLogRequest {
            source_id,
            location: SourceLocation::LocalPath(path.clone()),
            label: "count.log".to_string(),
            default_encoding: "UTF-8".to_string(),
        })
        .unwrap();

        let scan = SearchEngine::scan_current_log_matches(
            SearchTarget {
                source_id,
                label: "count.log".to_string(),
                path: path.display().to_string(),
                location: SourceLocation::LocalPath(path.clone()),
            },
            handle,
            SearchQuery::new("error".to_string()),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let _ = fs::remove_file(path);

        assert_eq!(scan.match_count, 3);
        assert_eq!(scan.matches.len(), 2);
        assert_eq!(scan.matches[0].match_ranges, vec![0..5, 6..11]);
    }

    /// 验证当前日志计数接口只返回次数和扫描行数，不需要保留命中结果列表。
    #[test]
    fn current_log_count_returns_occurrences_without_results() {
        let path = std::env::temp_dir().join(format!(
            "argus-current-count-only-test-{}-{}.log",
            std::process::id(),
            6
        ));
        fs::write(&path, "ERROR ERROR\ninfo\nerror\n").unwrap();
        let source_id = SourceId(11);
        let handle = LogFileReader::open(OpenLogRequest {
            source_id,
            location: SourceLocation::LocalPath(path.clone()),
            label: "count-only.log".to_string(),
            default_encoding: "UTF-8".to_string(),
        })
        .unwrap();

        let count = SearchEngine::count_current_log_matches(
            handle,
            SearchQuery::new("error".to_string()),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let _ = fs::remove_file(path);

        assert_eq!(count.match_count, 3);
        assert_eq!(count.scanned_lines, 3);
    }

    /// 验证“下一个”只扫描到最近命中就停止，不会像计数一样扫完整文件。
    #[test]
    fn current_log_next_navigation_stops_at_first_match() {
        let path = std::env::temp_dir().join(format!(
            "argus-current-next-test-{}-{}.log",
            std::process::id(),
            3
        ));
        let mut text = String::from("INFO start\nERROR first\n");
        for index in 0..5000 {
            text.push_str(&format!("INFO filler {index}\n"));
        }
        fs::write(&path, text).unwrap();
        let source_id = SourceId(8);
        let handle = LogFileReader::open(OpenLogRequest {
            source_id,
            location: SourceLocation::LocalPath(path.clone()),
            label: "next.log".to_string(),
            default_encoding: "UTF-8".to_string(),
        })
        .unwrap();

        let navigation = SearchEngine::find_current_log_match(
            SearchTarget {
                source_id,
                label: "next.log".to_string(),
                path: path.display().to_string(),
                location: SourceLocation::LocalPath(path.clone()),
            },
            handle,
            SearchQuery::new("error".to_string()),
            CurrentLogMatchPosition {
                line_number: 0,
                match_index: None,
            },
            CurrentLogMatchDirection::Next,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let _ = fs::remove_file(path);

        let result = navigation.result.unwrap();
        assert_eq!(result.line_number, 1);
        assert_eq!(navigation.active_range, Some(0..5));
        assert_eq!(
            navigation.position,
            Some(CurrentLogMatchPosition {
                line_number: 1,
                match_index: Some(0),
            })
        );
        assert!(navigation.scanned_lines < 5000);
    }

    /// 验证同一行存在多个命中时，“下一个”会在行内移动到下一个范围。
    #[test]
    fn current_log_next_navigation_moves_within_same_line() {
        let path = std::env::temp_dir().join(format!(
            "argus-current-inline-next-test-{}-{}.log",
            std::process::id(),
            4
        ));
        fs::write(&path, "ERROR one ERROR two\n").unwrap();
        let source_id = SourceId(9);
        let handle = LogFileReader::open(OpenLogRequest {
            source_id,
            location: SourceLocation::LocalPath(path.clone()),
            label: "inline.log".to_string(),
            default_encoding: "UTF-8".to_string(),
        })
        .unwrap();

        let navigation = SearchEngine::find_current_log_match(
            SearchTarget {
                source_id,
                label: "inline.log".to_string(),
                path: path.display().to_string(),
                location: SourceLocation::LocalPath(path.clone()),
            },
            handle,
            SearchQuery::new("error".to_string()),
            CurrentLogMatchPosition {
                line_number: 0,
                match_index: Some(0),
            },
            CurrentLogMatchDirection::Next,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let _ = fs::remove_file(path);

        assert_eq!(navigation.active_range, Some(10..15));
        assert_eq!(
            navigation.position,
            Some(CurrentLogMatchPosition {
                line_number: 0,
                match_index: Some(1),
            })
        );
        assert_eq!(navigation.scanned_lines, 1);
    }

    /// 验证“上一个”从文件开头命中处继续点击时会循环到最后一个命中。
    #[test]
    fn current_log_previous_navigation_wraps_to_last_match() {
        let path = std::env::temp_dir().join(format!(
            "argus-current-prev-test-{}-{}.log",
            std::process::id(),
            5
        ));
        fs::write(&path, "ERROR first\nINFO middle\nerror last\n").unwrap();
        let source_id = SourceId(10);
        let handle = LogFileReader::open(OpenLogRequest {
            source_id,
            location: SourceLocation::LocalPath(path.clone()),
            label: "prev.log".to_string(),
            default_encoding: "UTF-8".to_string(),
        })
        .unwrap();

        let navigation = SearchEngine::find_current_log_match(
            SearchTarget {
                source_id,
                label: "prev.log".to_string(),
                path: path.display().to_string(),
                location: SourceLocation::LocalPath(path.clone()),
            },
            handle,
            SearchQuery::new("error".to_string()),
            CurrentLogMatchPosition {
                line_number: 0,
                match_index: Some(0),
            },
            CurrentLogMatchDirection::Previous,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let _ = fs::remove_file(path);

        let result = navigation.result.unwrap();
        assert_eq!(result.line_number, 2);
        assert_eq!(navigation.active_range, Some(0..5));
    }

    /// 验证搜索引擎会扫描真实日志文件并返回全部匹配行。
    #[test]
    fn searches_real_file_and_reports_all_results() {
        let path = std::env::temp_dir().join(format!(
            "argus-search-test-{}-{}.log",
            std::process::id(),
            1
        ));
        fs::write(&path, "INFO start\nERROR failed\nwarn\nerror again\n").unwrap();
        let request = SearchRequest {
            generation: 1,
            scope: SearchScope::CurrentFile,
            query: SearchQuery::new("error".to_string()),
            targets: vec![SearchTarget {
                source_id: SourceId(1),
                label: "test.log".to_string(),
                path: path.display().to_string(),
                location: SourceLocation::LocalPath(path.clone()),
            }],
            default_encoding: "UTF-8".to_string(),
        };
        let mut batches = Vec::new();

        let summary = SearchEngine::search(
            request,
            |_| {},
            |results| batches.extend(results),
            Arc::new(AtomicBool::new(false)),
        );
        let _ = fs::remove_file(path);

        assert!(!summary.was_cancelled);
        assert_eq!(summary.matched_results, 2);
        assert_eq!(
            batches
                .iter()
                .map(|result| result.line_number)
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
    }
}
