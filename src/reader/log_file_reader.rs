//! 文件职责：实现日志文件统一读取器和分页文档模型。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：根据日志大小选择内存行文档或分页文档，避免超大日志被整体解码成单个字符串。

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, bail};

use crate::loader::SourceLocation;
use crate::loader::archive::ArchivePasswordStore;
use crate::reader::encoding_detector::{
    DecodedText, decode_log_bytes, decode_log_bytes_with_known_encoding,
};
use crate::reader::line_index::{
    LineIndex, LineIndexEntry, build_line_index_with_encoding, checked_line_span,
};
use crate::reader::mmap_backend::MmapBackend;
use crate::reader::spooled_backend::{SpoolCleanup, create_spool_file};
use crate::reader::stream_backend::ArchiveStreamBackend;

/// 超大日志分页阈值；超过该大小后不再整体解码到内存。
pub(crate) const LARGE_LOG_THRESHOLD_BYTES: u64 = 30 * 1024 * 1024;
/// 分页行缓存上限，避免大日志滚动后把已访问内容全部留在内存中。
const PAGED_DECODE_CACHE_LIMIT_BYTES: usize = 64 * 1024 * 1024;
/// 分页搜索批量读取字节上限；顺序扫描时按字节窗口合并行，避免逐行 seek/read。
const PAGED_SEARCH_BATCH_BYTES: u64 = 8 * 1024 * 1024;
/// 编码检测采样大小；大文件只读取开头样本判断编码。
const ENCODING_SAMPLE_BYTES: usize = 4 * 1024 * 1024;

/// 打开日志的请求模型，隔离 UI 状态和底层读取实现。
#[derive(Clone, Debug)]
pub(crate) struct OpenLogRequest {
    /// 来源位置，可能是本地文件，也可能是压缩包内部条目。
    pub location: SourceLocation,
    /// UI 展示名称。
    pub label: String,
    /// 用户设置的默认编码名称。
    pub default_encoding: String,
    /// 当前会话中已输入的压缩包密码快照；仅用于本次后台读取，不持久化。
    pub archive_passwords: ArchivePasswordStore,
}

/// 日志读取生命周期状态，存入应用状态供内容区和状态栏展示。
#[derive(Clone, Debug)]
pub(crate) enum LogOpenState {
    /// 尚未开始读取。
    Idle,
    /// 后台任务正在打开或索引日志。
    Loading {
        /// 读取提示，用于状态栏展示。
        message: String,
    },
    /// 日志已完成首轮读取并可显示文本。
    Ready(LogReaderHandle),
    /// 打开或读取失败。
    Failed {
        /// 错误详情。
        message: String,
    },
}

/// 打开后的日志读取句柄，保存统一文档模型并提供状态栏统计。
#[derive(Clone, Debug)]
pub(crate) struct LogReaderHandle {
    /// UI 展示名称。
    pub label: String,
    /// 来源路径展示文本。
    pub path: String,
    /// 当前日志文档。
    document: LogDocument,
}

impl LogReaderHandle {
    /// 返回当前日志文档。
    pub(crate) fn document(&self) -> &LogDocument {
        &self.document
    }

    /// 返回总行数。
    pub(crate) fn line_count(&self) -> usize {
        self.document.line_count()
    }

    /// 返回日志文本是否为空。
    pub(crate) fn is_empty(&self) -> bool {
        self.document.line_count() == 0
    }

    /// 读取指定范围内的日志行文本，供复制和 UI 可见区渲染复用。
    pub(crate) fn lines(
        &self,
        start_line: usize,
        max_lines: usize,
    ) -> Result<Vec<DisplayedLogLine>> {
        self.document.lines(start_line, max_lines)
    }

    /// 只读取已经命中的缓存行；分页日志缺失行不会触发文件 I/O。
    ///
    /// 参数说明：
    /// - `start_line`：起始 0 基行号。
    /// - `max_lines`：最大读取行数。
    ///
    /// 返回值：与请求范围等长的行槽位；`None` 表示该行尚未进入分页缓存。
    pub(crate) fn cached_lines(
        &self,
        start_line: usize,
        max_lines: usize,
    ) -> Vec<Option<DisplayedLogLine>> {
        self.document.cached_lines(start_line, max_lines)
    }

    /// 判断指定行范围是否已经完整进入缓存，供 UI 决定是否需要后台预取。
    pub(crate) fn has_cached_lines(&self, start_line: usize, max_lines: usize) -> bool {
        self.document
            .cached_lines(start_line, max_lines)
            .iter()
            .all(Option::is_some)
    }

    /// 返回用于横向滚动估算的最长显示列数，不在分页文档中读取正文。
    pub(crate) fn estimated_longest_display_columns(&self) -> usize {
        self.document.estimated_longest_display_columns()
    }
}

/// UI 可展示的日志文档；小文件在内存中，大文件按页读取。
#[derive(Clone, Debug)]
pub(crate) enum LogDocument {
    /// 小日志完整拆行为内存行列表。
    InMemory(InMemoryLogDocument),
    /// 大日志只保存行索引，正文按需读取。
    Paged(PagedLogDocument),
}

impl LogDocument {
    /// 返回日志行数。
    pub(crate) fn line_count(&self) -> usize {
        match self {
            Self::InMemory(document) => document.line_count(),
            Self::Paged(document) => document.line_count(),
        }
    }

    /// 按范围读取日志行。
    pub(crate) fn lines(
        &self,
        start_line: usize,
        max_lines: usize,
    ) -> Result<Vec<DisplayedLogLine>> {
        match self {
            Self::InMemory(document) => Ok(document
                .lines
                .iter()
                .enumerate()
                .skip(start_line)
                .take(max_lines)
                .map(|(line_number, text)| DisplayedLogLine {
                    line_number,
                    text: text.clone(),
                })
                .collect()),
            Self::Paged(document) => {
                document
                    .read_visible_lines(start_line, max_lines)
                    .map(|lines| {
                        lines
                            .into_iter()
                            .map(|line| DisplayedLogLine {
                                line_number: line.line_number,
                                text: line.text.to_string(),
                            })
                            .collect()
                    })
            }
        }
    }

    /// 只从现有缓存读取日志行；分页文档不会在 UI 渲染期访问文件。
    fn cached_lines(&self, start_line: usize, max_lines: usize) -> Vec<Option<DisplayedLogLine>> {
        match self {
            Self::InMemory(document) => document
                .lines
                .iter()
                .enumerate()
                .skip(start_line)
                .take(max_lines)
                .map(|(line_number, text)| {
                    Some(DisplayedLogLine {
                        line_number,
                        text: text.clone(),
                    })
                })
                .collect(),
            Self::Paged(document) => document
                .cached_visible_lines(start_line, max_lines)
                .into_iter()
                .map(|line| {
                    line.map(|line| DisplayedLogLine {
                        line_number: line.line_number,
                        text: line.text.to_string(),
                    })
                })
                .collect(),
        }
    }

    /// 返回最长显示列数估算；分页文档只使用索引元信息，避免 UI 渲染路径读取正文。
    pub(crate) fn estimated_longest_display_columns(&self) -> usize {
        match self {
            Self::InMemory(document) => document.longest_display_columns,
            Self::Paged(document) => document.estimated_longest_display_columns(),
        }
    }
}

/// 小日志内存文档。
#[derive(Clone, Debug)]
pub(crate) struct InMemoryLogDocument {
    /// 已解码行列表，不包含行尾换行符。
    pub lines: Arc<Vec<String>>,
    /// 最长展示列数，读取阶段预计算，避免 UI 切换页签时重新扫描长行。
    pub longest_display_columns: usize,
}

impl InMemoryLogDocument {
    /// 返回日志行数。
    pub(crate) fn line_count(&self) -> usize {
        self.lines.len()
    }
}

/// 分页读取后返回的一行日志。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PagedLine {
    /// 0 基行号。
    pub line_number: usize,
    /// 已解码正文，不包含行尾换行符。
    pub text: Arc<str>,
    /// 原始字节偏移。
    pub byte_offset: u64,
}

/// UI 展示用日志行。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DisplayedLogLine {
    /// 0 基行号。
    pub line_number: usize,
    /// 行正文。
    pub text: String,
}

/// 大日志分页文档。
#[derive(Clone, Debug)]
pub(crate) struct PagedLogDocument {
    /// 可 seek 的本地文件路径；可能是真实日志，也可能是压缩流物化文件。
    path: PathBuf,
    /// 实际采用的编码标签。
    encoding: String,
    /// 运行期逐行自修复得到的编码；用于样本误判后避免每一行重复自动检测。
    encoding_override: Arc<Mutex<Option<String>>>,
    /// 用户配置的兜底编码，用于逐行解码。
    preferred_encoding: String,
    /// 行号到字节范围索引。
    line_index: LineIndex,
    /// 解码行缓存。
    cache: Arc<Mutex<PagedLineCache>>,
    /// 共享文件句柄，避免滚动过程中反复打开文件。
    file_handle: Arc<Mutex<Option<File>>>,
    /// 压缩日志临时分页文件清理器；本地文件为 `None`。
    _spool_cleanup: Option<Arc<SpoolCleanup>>,
}

impl PagedLogDocument {
    /// 从可 seek 本地路径创建分页文档。
    ///
    /// 参数说明：
    /// - `path`：真实日志或已物化临时日志路径。
    /// - `preferred_encoding`：用户配置的兜底编码。
    /// - `spool_cleanup`：压缩日志临时文件清理器，本地文件传 `None`。
    ///
    /// 返回值：可按行分页读取的文档。
    pub(crate) fn open(
        path: PathBuf,
        preferred_encoding: String,
        spool_cleanup: Option<Arc<SpoolCleanup>>,
    ) -> Result<Self> {
        let encoding = detect_encoding_from_file_sample(&path, &preferred_encoding)?;
        let line_index = build_line_index_with_encoding(&path, &encoding)?;

        Ok(Self {
            path,
            encoding,
            encoding_override: Arc::new(Mutex::new(None)),
            preferred_encoding,
            line_index,
            cache: Arc::new(Mutex::new(PagedLineCache::new(
                PAGED_DECODE_CACHE_LIMIT_BYTES,
            ))),
            file_handle: Arc::new(Mutex::new(None)),
            _spool_cleanup: spool_cleanup,
        })
    }

    /// 返回日志行数。
    pub(crate) fn line_count(&self) -> usize {
        self.line_index.len()
    }

    /// 返回最长显示列数估算，不读取最长行正文。
    pub(crate) fn estimated_longest_display_columns(&self) -> usize {
        let byte_len = self.line_index.longest_line_byte_len() as usize;
        let encoding = self.effective_encoding_label();
        if encoding.eq_ignore_ascii_case("UTF-16LE") || encoding.eq_ignore_ascii_case("UTF-16BE") {
            byte_len / 2
        } else {
            byte_len
        }
    }

    /// 读取单行日志。
    pub(crate) fn read_line(&self, line_number: usize) -> Result<Option<PagedLine>> {
        self.read_visible_lines(line_number, 1)
            .map(|mut lines| lines.pop())
    }

    /// 批量读取连续可见行，缓存命中时不访问文件。
    ///
    /// 参数说明：
    /// - `start_line`：起始 0 基行号。
    /// - `max_lines`：最大读取行数，通常为当前视口容量。
    ///
    /// 返回值：按行号升序排列的解码行。
    pub(crate) fn read_visible_lines(
        &self,
        start_line: usize,
        max_lines: usize,
    ) -> Result<Vec<PagedLine>> {
        if max_lines == 0 || start_line >= self.line_index.len() {
            return Ok(Vec::new());
        }

        let end_line = start_line
            .saturating_add(max_lines)
            .min(self.line_index.len());
        let mut ordered = Vec::with_capacity(end_line - start_line);
        let mut missing = Vec::new();

        if let Ok(mut cache) = self.cache.lock() {
            for line_number in start_line..end_line {
                if let Some(line) = cache.get(line_number) {
                    ordered.push(Some(line));
                } else {
                    ordered.push(None);
                    missing.push(line_number);
                }
            }
        } else {
            for line_number in start_line..end_line {
                ordered.push(None);
                missing.push(line_number);
            }
        }

        if !missing.is_empty() {
            let decoded = self.read_missing_lines(&missing)?;
            if let Ok(mut cache) = self.cache.lock() {
                for line in &decoded {
                    cache.insert(line.clone());
                }
            }

            let mut decoded_iter = decoded.into_iter();
            for slot in &mut ordered {
                if slot.is_none() {
                    *slot = decoded_iter.next();
                }
            }
        }

        Ok(ordered.into_iter().flatten().collect())
    }

    /// 只读取分页缓存中的连续可见行，缓存缺失时返回空槽位并且不访问文件。
    pub(crate) fn cached_visible_lines(
        &self,
        start_line: usize,
        max_lines: usize,
    ) -> Vec<Option<PagedLine>> {
        if max_lines == 0 || start_line >= self.line_index.len() {
            return Vec::new();
        }

        let end_line = start_line
            .saturating_add(max_lines)
            .min(self.line_index.len());
        let Ok(mut cache) = self.cache.lock() else {
            return (start_line..end_line).map(|_| None).collect();
        };

        (start_line..end_line)
            .map(|line_number| cache.get(line_number))
            .collect()
    }

    /// 按行号升序遍历分页日志行，读取时按连续字节窗口合并 I/O。
    ///
    /// 参数说明：
    /// - `line_range`：需要遍历的 0 基行号范围，超出文档行数会自动裁剪。
    /// - `callback`：每解码一行调用；返回 `false` 表示调用方已找到目标，可以提前停止。
    ///
    /// 返回值：`true` 表示完整遍历，`false` 表示被回调提前停止。
    pub(crate) fn for_each_line_in_range<F>(
        &self,
        line_range: Range<usize>,
        mut callback: F,
    ) -> Result<bool>
    where
        F: FnMut(DisplayedLogLine) -> bool,
    {
        let range_end = line_range.end.min(self.line_index.len());
        let mut current_line = line_range.start.min(range_end);

        while current_line < range_end {
            let Some((batch_start, batch_end, batch_offset, batch_len)) =
                self.next_forward_scan_batch(current_line, range_end)?
            else {
                return Ok(true);
            };
            let span = self.read_byte_span(batch_offset, batch_len)?;

            for line_number in batch_start..batch_end {
                let Some(line) =
                    self.decode_displayed_line_from_batch(line_number, batch_offset, &span)?
                else {
                    continue;
                };
                if !callback(line) {
                    return Ok(false);
                }
            }

            current_line = batch_end;
        }

        Ok(true)
    }

    /// 按行号降序遍历分页日志行，服务“上一个”这类反向定位场景。
    ///
    /// 参数说明：
    /// - `line_range`：需要遍历的 0 基行号范围，超出文档行数会自动裁剪。
    /// - `callback`：每解码一行调用；返回 `false` 表示调用方已找到目标，可以提前停止。
    ///
    /// 返回值：`true` 表示完整遍历，`false` 表示被回调提前停止。
    pub(crate) fn for_each_line_in_range_rev<F>(
        &self,
        line_range: Range<usize>,
        mut callback: F,
    ) -> Result<bool>
    where
        F: FnMut(DisplayedLogLine) -> bool,
    {
        let range_start = line_range.start.min(self.line_index.len());
        let mut current_end = line_range.end.min(self.line_index.len());

        while current_end > range_start {
            let Some((batch_start, batch_end, batch_offset, batch_len)) =
                self.next_reverse_scan_batch(range_start, current_end)?
            else {
                return Ok(true);
            };
            let span = self.read_byte_span(batch_offset, batch_len)?;

            for line_number in (batch_start..batch_end).rev() {
                let Some(line) =
                    self.decode_displayed_line_from_batch(line_number, batch_offset, &span)?
                else {
                    continue;
                };
                if !callback(line) {
                    return Ok(false);
                }
            }

            current_end = batch_start;
        }

        Ok(true)
    }

    /// 读取未命中的连续可见范围，并逐行解码。
    fn read_missing_lines(&self, missing: &[usize]) -> Result<Vec<PagedLine>> {
        let first_line = missing[0];
        let last_line = *missing
            .last()
            .ok_or_else(|| anyhow::anyhow!("分页日志缺失行范围为空"))?;
        let first_entry = self
            .line_index
            .get(first_line)
            .ok_or_else(|| anyhow::anyhow!("日志行 {} 不存在", first_line + 1))?;
        let last_entry = self
            .line_index
            .get(last_line)
            .ok_or_else(|| anyhow::anyhow!("日志行 {} 不存在", last_line + 1))?;
        let (span_offset, span_len) = checked_line_span(first_entry, last_entry)?;
        let span = self.read_byte_span(span_offset, span_len)?;
        let mut decoded = Vec::with_capacity(missing.len());

        for line_number in missing {
            let entry = self
                .line_index
                .get(*line_number)
                .ok_or_else(|| anyhow::anyhow!("日志行 {} 不存在", line_number + 1))?;
            decoded.push(self.decode_line_from_span(*line_number, entry, first_entry, &span)?);
        }

        Ok(decoded)
    }

    /// 从共享文件句柄读取指定原始字节范围。
    fn read_byte_span(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        let mut handle_guard = self
            .file_handle
            .lock()
            .map_err(|_| anyhow::anyhow!("分页日志文件句柄被占用，暂时无法读取"))?;
        if handle_guard.is_none() {
            *handle_guard = Some(
                File::open(&self.path)
                    .with_context(|| format!("无法打开分页日志：{}", self.path.display()))?,
            );
        }
        let handle = handle_guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("分页日志文件句柄初始化失败"))?;
        handle
            .seek(SeekFrom::Start(offset))
            .with_context(|| format!("无法定位分页日志：{}", self.path.display()))?;
        let mut bytes = vec![0_u8; len as usize];
        handle
            .read_exact(&mut bytes)
            .with_context(|| format!("无法读取分页日志：{}", self.path.display()))?;
        Ok(bytes)
    }

    /// 从合并读取的 span 中切出单行并解码。
    fn decode_line_from_span(
        &self,
        line_number: usize,
        entry: LineIndexEntry,
        first_entry: LineIndexEntry,
        span: &[u8],
    ) -> Result<PagedLine> {
        let relative_offset = entry
            .offset
            .checked_sub(first_entry.offset)
            .ok_or_else(|| anyhow::anyhow!("分页日志行偏移无效"))?;
        let start = usize::try_from(relative_offset)
            .map_err(|_| anyhow::anyhow!("分页日志行偏移过大，无法读取"))?;
        let end = start
            .checked_add(entry.byte_len as usize)
            .ok_or_else(|| anyhow::anyhow!("分页日志行长度溢出"))?;
        let bytes = span
            .get(start..end)
            .ok_or_else(|| anyhow::anyhow!("分页日志行范围超出读取结果"))?;
        let decoded = self.decode_line_bytes(bytes);

        Ok(PagedLine {
            line_number,
            text: Arc::from(decoded.text),
            byte_offset: entry.offset,
        })
    }

    /// 计算正向搜索的下一批连续字节窗口。
    fn next_forward_scan_batch(
        &self,
        current_line: usize,
        range_end: usize,
    ) -> Result<Option<(usize, usize, u64, u64)>> {
        let Some(first_entry) = self.line_index.get(current_line) else {
            return Ok(None);
        };
        let batch_start = current_line;
        let mut batch_end = current_line + 1;
        let batch_offset = first_entry.offset;
        let mut batch_len = u64::from(first_entry.byte_len);

        while batch_end < range_end {
            let Some(next_entry) = self.line_index.get(batch_end) else {
                break;
            };
            let next_end = next_entry
                .offset
                .checked_add(u64::from(next_entry.byte_len))
                .ok_or_else(|| anyhow::anyhow!("分页日志搜索批次范围溢出"))?;
            let candidate_len = next_end
                .checked_sub(batch_offset)
                .ok_or_else(|| anyhow::anyhow!("分页日志搜索批次范围无效"))?;
            if candidate_len > PAGED_SEARCH_BATCH_BYTES && batch_end > batch_start + 1 {
                break;
            }
            batch_len = candidate_len;
            batch_end += 1;
            if candidate_len >= PAGED_SEARCH_BATCH_BYTES {
                break;
            }
        }

        Ok(Some((batch_start, batch_end, batch_offset, batch_len)))
    }

    /// 计算反向搜索的下一批连续字节窗口。
    fn next_reverse_scan_batch(
        &self,
        range_start: usize,
        current_end: usize,
    ) -> Result<Option<(usize, usize, u64, u64)>> {
        if current_end <= range_start {
            return Ok(None);
        }

        let last_line = current_end - 1;
        let Some(last_entry) = self.line_index.get(last_line) else {
            return Ok(None);
        };
        let batch_end = current_end;
        let mut batch_start = last_line;
        let last_byte_end = last_entry
            .offset
            .checked_add(u64::from(last_entry.byte_len))
            .ok_or_else(|| anyhow::anyhow!("分页日志搜索批次范围溢出"))?;
        let mut batch_offset = last_entry.offset;
        let mut batch_len = u64::from(last_entry.byte_len);

        while batch_start > range_start {
            let previous_line = batch_start - 1;
            let Some(previous_entry) = self.line_index.get(previous_line) else {
                break;
            };
            let candidate_len = last_byte_end
                .checked_sub(previous_entry.offset)
                .ok_or_else(|| anyhow::anyhow!("分页日志搜索批次范围无效"))?;
            if candidate_len > PAGED_SEARCH_BATCH_BYTES && batch_start + 1 < batch_end {
                break;
            }
            batch_start = previous_line;
            batch_offset = previous_entry.offset;
            batch_len = candidate_len;
            if candidate_len >= PAGED_SEARCH_BATCH_BYTES {
                break;
            }
        }

        Ok(Some((batch_start, batch_end, batch_offset, batch_len)))
    }

    /// 从搜索批次字节中切出并解码一行展示文本。
    fn decode_displayed_line_from_batch(
        &self,
        line_number: usize,
        batch_offset: u64,
        span: &[u8],
    ) -> Result<Option<DisplayedLogLine>> {
        let Some(entry) = self.line_index.get(line_number) else {
            return Ok(None);
        };
        let relative_offset = entry
            .offset
            .checked_sub(batch_offset)
            .ok_or_else(|| anyhow::anyhow!("分页日志搜索行偏移无效"))?;
        let start = usize::try_from(relative_offset)
            .map_err(|_| anyhow::anyhow!("分页日志搜索行偏移过大"))?;
        let end = start
            .checked_add(entry.byte_len as usize)
            .ok_or_else(|| anyhow::anyhow!("分页日志搜索行长度溢出"))?;
        let bytes = span
            .get(start..end)
            .ok_or_else(|| anyhow::anyhow!("分页日志搜索行范围超出读取结果"))?;
        let decoded = self.decode_line_bytes(bytes);

        Ok(Some(DisplayedLogLine {
            line_number,
            text: decoded.text,
        }))
    }

    /// 返回当前分页文档应使用的编码标签；运行期修复优先于初始样本结果。
    fn effective_encoding_label(&self) -> String {
        self.encoding_override
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
            .unwrap_or_else(|| self.encoding.clone())
    }

    /// 解码单行字节，并在发现更可靠编码时更新分页文档运行期编码缓存。
    fn decode_line_bytes(&self, bytes: &[u8]) -> DecodedText {
        let previous_encoding = self.effective_encoding_label();
        let decoded = decode_log_bytes_with_known_encoding(
            bytes,
            &previous_encoding,
            &self.preferred_encoding,
        );
        if !decoded
            .encoding_label
            .eq_ignore_ascii_case(previous_encoding.as_str())
        {
            self.promote_runtime_encoding(decoded.encoding_label.as_str());
        }
        decoded
    }

    /// 记录逐行解码自修复出的编码，并清空旧编码下的缓存行。
    fn promote_runtime_encoding(&self, encoding_label: &str) {
        let mut changed = false;
        if let Ok(mut guard) = self.encoding_override.lock() {
            let current = guard.as_deref().unwrap_or(self.encoding.as_str());
            if !encoding_label.eq_ignore_ascii_case(current) {
                *guard = Some(encoding_label.to_string());
                changed = true;
            }
        }

        if changed {
            // 旧缓存可能包含误判编码下的乱码行；切换运行期编码后必须丢弃。
            if let Ok(mut cache) = self.cache.lock() {
                cache.clear();
            }
        }
    }
}

/// 简单行缓存；按插入顺序淘汰，避免在 UI 渲染热路径中为每次命中重排缓存。
#[derive(Debug)]
struct PagedLineCache {
    /// 最大缓存字节数。
    limit_bytes: usize,
    /// 当前缓存估算字节数。
    used_bytes: usize,
    /// 行号到已解码行。
    lines: HashMap<usize, PagedLine>,
    /// 插入顺序，队首为最早进入缓存的行。
    order: VecDeque<usize>,
}

impl PagedLineCache {
    /// 创建缓存。
    fn new(limit_bytes: usize) -> Self {
        Self {
            limit_bytes,
            used_bytes: 0,
            lines: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// 读取缓存行，并刷新最近使用顺序。
    fn get(&mut self, line_number: usize) -> Option<PagedLine> {
        let line = self.lines.get(&line_number).cloned()?;
        self.order.retain(|stored| *stored != line_number);
        self.order.push_back(line_number);
        Some(line)
    }

    /// 插入缓存行，并按总字节数淘汰旧行。
    fn insert(&mut self, line: PagedLine) {
        let line_number = line.line_number;
        if let Some(old) = self.lines.insert(line_number, line) {
            self.used_bytes = self.used_bytes.saturating_sub(old.text.len());
            if let Some(current) = self.lines.get(&line_number) {
                self.used_bytes = self.used_bytes.saturating_add(current.text.len());
            }
            self.evict_if_needed();
            return;
        }

        if let Some(current) = self.lines.get(&line_number) {
            self.used_bytes = self.used_bytes.saturating_add(current.text.len());
        }
        self.order.push_back(line_number);
        self.evict_if_needed();
    }

    /// 清空所有缓存行；用于分页文档运行期编码被修复后丢弃旧解码结果。
    fn clear(&mut self) {
        self.used_bytes = 0;
        self.lines.clear();
        self.order.clear();
    }

    /// 淘汰最久未使用的行；重复 order 项会自然跳过。
    fn evict_if_needed(&mut self) {
        while self.used_bytes > self.limit_bytes {
            let Some(line_number) = self.order.pop_front() else {
                break;
            };
            let Some(line) = self.lines.remove(&line_number) else {
                continue;
            };
            self.used_bytes = self.used_bytes.saturating_sub(line.text.len());
        }
    }
}

/// 日志文件读取器入口；所有后端选择逻辑集中在这里，UI 不直接 match 来源类型。
#[derive(Debug, Default)]
pub(crate) struct LogFileReader;

impl LogFileReader {
    /// 打开指定来源并返回可按行显示的日志句柄。
    ///
    /// 参数说明：
    /// - `request`：包含来源 ID、位置、标签和默认编码。
    ///
    /// 返回值：读取成功的句柄；失败时带上下文错误。
    pub(crate) fn open(request: OpenLogRequest) -> Result<LogReaderHandle> {
        match &request.location {
            SourceLocation::LocalPath(path) => open_local_log(request.clone(), path),
            SourceLocation::ArchiveEntry { .. } => open_archive_log(request),
        }
    }
}

/// 打开本地普通日志文件，根据大小选择内存或分页。
fn open_local_log(request: OpenLogRequest, path: &Path) -> Result<LogReaderHandle> {
    if !path.is_file() {
        bail!("日志来源不是普通文件：{}", path.display());
    }
    let metadata =
        std::fs::metadata(path).with_context(|| format!("无法读取日志文件：{}", path.display()))?;
    if metadata.len() > LARGE_LOG_THRESHOLD_BYTES {
        return build_paged_handle(request, path.to_path_buf(), None, metadata.len());
    }

    let bytes = MmapBackend::read_to_bytes(path)?;
    build_memory_handle(request, bytes)
}

/// 打开压缩包内部日志；小日志保存在内存，超阈值日志物化后分页。
fn open_archive_log(request: OpenLogRequest) -> Result<LogReaderHandle> {
    let mut bytes = Vec::new();
    let mut spooled_file: Option<File> = None;
    let mut spooled_path: Option<PathBuf> = None;
    let mut total_bytes = 0_u64;
    let label = request.label.clone();

    let stream_result = ArchiveStreamBackend::stream_to_consumer(
        &request.location,
        &request.archive_passwords,
        &mut |chunk| {
            total_bytes = total_bytes.saturating_add(chunk.len() as u64);
            if spooled_file.is_none() && total_bytes > LARGE_LOG_THRESHOLD_BYTES {
                let (mut file, path) = create_spool_file(&label)?;
                file.write_all(&bytes)
                    .with_context(|| format!("无法写入日志分页缓存：{}", path.display()))?;
                bytes.clear();
                spooled_file = Some(file);
                spooled_path = Some(path);
            }

            if let Some(file) = spooled_file.as_mut() {
                file.write_all(chunk).context("无法写入压缩日志分页缓存")?;
            } else {
                bytes.extend_from_slice(chunk);
            }

            Ok(())
        },
    );

    if let Err(error) = stream_result {
        drop(spooled_file.take());
        if let Some(path) = spooled_path.take() {
            let _ = fs::remove_file(path);
        }
        return Err(error);
    }

    if let Some(mut file) = spooled_file {
        let path = spooled_path.ok_or_else(|| anyhow::anyhow!("压缩日志分页缓存路径缺失"))?;
        let cleanup = SpoolCleanup::new(path.clone());
        if let Err(error) = file.flush().context("无法刷新压缩日志分页缓存") {
            drop(file);
            drop(cleanup);
            return Err(error);
        }
        drop(file);
        return build_paged_handle(request, path, Some(cleanup), total_bytes);
    }

    build_memory_handle(request, bytes)
}

/// 从完整字节构建内存行文档。
fn build_memory_handle(request: OpenLogRequest, bytes: Vec<u8>) -> Result<LogReaderHandle> {
    let decoded = decode_log_bytes(&bytes, &request.default_encoding);
    let lines = split_decoded_lines(&decoded.text);
    let longest_line_index = longest_line_index(&lines);
    let longest_display_columns = lines
        .get(longest_line_index)
        .map(|line| display_column_count(line))
        .unwrap_or_default();
    let document = LogDocument::InMemory(InMemoryLogDocument {
        lines: Arc::new(lines),
        longest_display_columns,
    });

    Ok(LogReaderHandle {
        label: request.label,
        path: request.location.display_path(),
        document,
    })
}

/// 从本地可 seek 文件构建分页文档句柄。
fn build_paged_handle(
    request: OpenLogRequest,
    path: PathBuf,
    cleanup: Option<Arc<SpoolCleanup>>,
    _total_bytes: u64,
) -> Result<LogReaderHandle> {
    let document = LogDocument::Paged(PagedLogDocument::open(
        path,
        request.default_encoding.clone(),
        cleanup,
    )?);

    Ok(LogReaderHandle {
        label: request.label,
        path: request.location.display_path(),
        document,
    })
}

/// 从大文件开头样本检测编码。
fn detect_encoding_from_file_sample(path: &Path, preferred_encoding: &str) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("无法读取日志编码样本：{}", path.display()))?;
    let mut bytes = vec![0_u8; ENCODING_SAMPLE_BYTES];
    let read = file
        .read(&mut bytes)
        .with_context(|| format!("无法读取日志编码样本：{}", path.display()))?;
    bytes.truncate(read);
    Ok(decode_log_bytes(&bytes, preferred_encoding).encoding_label)
}

/// 将解码后的完整文本拆为行，不把末尾换行额外显示为空行。
fn split_decoded_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::new();
    let mut start = 0_usize;
    let bytes = text.as_bytes();
    let mut index = 0_usize;
    while index < bytes.len() {
        match bytes[index] {
            b'\n' => {
                lines.push(text[start..index].to_string());
                index += 1;
                start = index;
            }
            b'\r' => {
                lines.push(text[start..index].to_string());
                index += 1;
                if bytes.get(index) == Some(&b'\n') {
                    index += 1;
                }
                start = index;
            }
            _ => index += 1,
        }
    }
    if start < text.len() {
        lines.push(text[start..].to_string());
    }

    lines
}

/// 返回最长行索引。
fn longest_line_index(lines: &[String]) -> usize {
    lines
        .iter()
        .enumerate()
        .max_by_key(|(_, line)| line.len())
        .map(|(index, _)| index)
        .unwrap_or(0)
}

/// 返回展示列估算；制表符按日志阅读区规则扩展为 4 列。
fn display_column_count(line: &str) -> usize {
    line.chars()
        .map(|character| if character == '\t' { 4 } else { 1 })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::{
        LARGE_LOG_THRESHOLD_BYTES, LogDocument, LogFileReader, OpenLogRequest, PagedLogDocument,
        split_decoded_lines,
    };
    use crate::loader::SourceLocation;
    use crate::loader::archive::{ArchiveFormat, ArchivePasswordStore};
    use std::fs;
    use std::io::{Seek, SeekFrom, Write};
    use std::path::PathBuf;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    /// 构造隔离的临时日志路径，避免单元测试依赖真实用户目录。
    fn temp_log_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("argus-reader-{}-{name}", std::process::id()))
    }

    /// 验证 LF、CRLF、CR 和末尾无换行均能正确拆行。
    #[test]
    fn splits_lines_for_virtual_viewer() {
        let lines = split_decoded_lines("a\r\n[INFO] b\nc\rd");

        assert_eq!(lines, vec!["a", "[INFO] b", "c", "d"]);
        assert!(split_decoded_lines("").is_empty());
        assert_eq!(split_decoded_lines("a\n"), vec!["a"]);
    }

    /// 验证空文件打开成功并返回 0 行。
    #[test]
    fn opens_empty_local_file() {
        let path = temp_log_path("empty.log");
        fs::write(&path, []).expect("应能写入空日志文件");

        let handle = LogFileReader::open(OpenLogRequest {
            location: SourceLocation::LocalPath(path.clone()),
            label: "empty.log".to_string(),
            default_encoding: "UTF-8".to_string(),
            archive_passwords: ArchivePasswordStore::default(),
        })
        .expect("空日志文件应能打开");

        assert_eq!(handle.line_count(), 0);
        assert!(matches!(handle.document(), LogDocument::InMemory(_)));

        let _ = fs::remove_file(path);
    }

    /// 验证超过阈值的本地文件进入分页模式，不构造完整文本。
    #[test]
    fn opens_large_local_file_as_paged_document() {
        let path = temp_log_path("large.log");
        let mut file = fs::File::create(&path).expect("应能创建大日志文件");
        file.write_all(b"first line\n").expect("应能写入首行");
        file.seek(SeekFrom::Start(LARGE_LOG_THRESHOLD_BYTES + 16))
            .expect("应能创建稀疏文件");
        file.write_all(b"last line").expect("应能写入尾行");
        drop(file);

        let handle = LogFileReader::open(OpenLogRequest {
            location: SourceLocation::LocalPath(path.clone()),
            label: "large.log".to_string(),
            default_encoding: "UTF-8".to_string(),
            archive_passwords: ArchivePasswordStore::default(),
        })
        .expect("大日志应能以分页模式打开");

        assert!(matches!(handle.document(), LogDocument::Paged(_)));
        assert_eq!(handle.lines(0, 1).unwrap()[0].text, "first line");

        let _ = fs::remove_file(path);
    }

    /// 验证分页文档按已检测编码解码 UTF-16LE 行，避免后续行因缺少 BOM 变成带 NUL 文本。
    #[test]
    fn paged_document_reads_utf16le_lines() {
        let path = temp_log_path("utf16le.log");
        let mut bytes = vec![0xff, 0xfe];
        for unit in "first\n第二\r\nlast".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        fs::write(&path, bytes).expect("应能写入 UTF-16LE 日志");

        let document = PagedLogDocument::open(path.clone(), "UTF-8".to_string(), None)
            .expect("应能打开分页文档");
        let lines = document
            .read_visible_lines(0, 3)
            .expect("应能读取 UTF-16LE 分页行");

        assert_eq!(lines[0].text.as_ref(), "first");
        assert_eq!(lines[1].text.as_ref(), "第二");
        assert_eq!(lines[2].text.as_ref(), "last");

        let _ = fs::remove_file(path);
    }

    /// 验证分页搜索遍历能按行号正反向读取，并支持调用方提前停止。
    #[test]
    fn paged_document_iterates_search_ranges_with_early_stop() {
        let path = temp_log_path("paged-search-range.log");
        fs::write(&path, "alpha\nbeta\nerror\nomega\n").expect("应能写入分页遍历测试日志");
        let document = PagedLogDocument::open(path.clone(), "UTF-8".to_string(), None)
            .expect("应能打开分页遍历测试日志");
        let mut forward = Vec::new();

        let completed = document
            .for_each_line_in_range(1..4, |line| {
                forward.push((line.line_number, line.text));
                forward.len() < 2
            })
            .expect("正向分页遍历应能读取");

        assert!(!completed);
        assert_eq!(
            forward,
            vec![(1, "beta".to_string()), (2, "error".to_string())]
        );

        let mut backward = Vec::new();
        let completed = document
            .for_each_line_in_range_rev(0..3, |line| {
                backward.push((line.line_number, line.text));
                backward.len() < 2
            })
            .expect("反向分页遍历应能读取");

        assert!(!completed);
        assert_eq!(
            backward,
            vec![(2, "error".to_string()), (1, "beta".to_string())]
        );

        let _ = fs::remove_file(path);
    }

    /// 验证压缩包内小日志通过压缩适配器流式入口打开，不需要先解包到临时文件。
    #[test]
    fn opens_zip_log_entry_with_archive_streaming_mode() {
        let path = temp_log_path("archive.zip");
        let file = fs::File::create(&path).expect("应能创建 ZIP 测试文件");
        let mut writer = ZipWriter::new(file);
        writer
            .start_file("logs/app.log", SimpleFileOptions::default())
            .expect("应能创建 ZIP 内日志条目");
        writer
            .write_all(b"[INFO] started\n[ERROR] failed")
            .expect("应能写入 ZIP 内日志内容");
        writer.finish().expect("应能完成 ZIP 写入");

        let handle = LogFileReader::open(OpenLogRequest {
            location: SourceLocation::ArchiveEntry {
                archive_path: path.clone(),
                root_format: ArchiveFormat::Zip,
                container_entries: Vec::new(),
                entry_path: "logs/app.log".to_string(),
                format: ArchiveFormat::Zip,
                archive_depth: 0,
            },
            label: "app.log".to_string(),
            default_encoding: "UTF-8".to_string(),
            archive_passwords: ArchivePasswordStore::default(),
        })
        .expect("ZIP 内日志应能直接读取");

        assert_eq!(handle.line_count(), 2);
        assert_eq!(handle.lines(0, 2).unwrap()[1].text, "[ERROR] failed");

        let _ = fs::remove_file(path);
    }
}
