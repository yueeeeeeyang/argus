//! 文件职责：为 AI Agent 提供独立于界面状态的日志目录索引与只读访问接口。
//! 创建日期：2026-07-17
//! 修改日期：2026-07-17
//! 作者：Argus 开发团队
//! 主要功能：预构建来源引用和目录前缀索引、生成完整目录清单、合并同源并发打开并复用会话读取器。

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::File;
use std::ops::Range;
use std::sync::{
    Arc, Mutex, OnceLock, Weak,
    atomic::{AtomicBool, Ordering},
};

use anyhow::{Result, anyhow, bail};
use memmap2::{Mmap, MmapOptions};

use crate::agent::session::{SnapshotSource, SourceScopeSnapshot};
use crate::loader::SourceLocation;
use crate::log_io::log_file_reader::{LogFileReader, LogReaderHandle, OpenLogRequest};

/// 会话内最多保留的完整 Agent 日志文档数量，防止大日志正文长期无界驻留。
const MAX_AGENT_LOG_CACHE_ENTRIES: usize = 2;
/// 本地文件读取块大小；每块完成后检查取消状态。
const AGENT_FILE_READ_CHUNK_BYTES: usize = 1024 * 1024;

/// Agent 目录中的一个文件夹汇总；只包含模型安全的展示路径和元数据。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentLogDirectorySummary {
    /// 多根来源快照内的唯一展示路径。
    pub path: String,
    /// 该目录直接包含的日志文件数量。
    pub direct_file_count: usize,
    /// 该目录全部后代日志文件数量。
    pub descendant_file_count: usize,
    /// 后代中大小已知的日志总字节数。
    pub known_total_bytes: u64,
}

/// 一次 Agent 日志打开结果；调用方据此只核算真实发生的首次扫描量。
#[derive(Clone, Debug)]
pub(crate) struct AgentOpenedLog {
    /// Agent 自己维护的不可变日志文档，不包含任何 UI 阅读状态。
    pub reader: AgentLogDocument,
    /// 是否直接命中会话读取器缓存或等待同源打开后复用其结果。
    pub cache_hit: bool,
}

/// Agent 工具读取的一行日志；行号保持 0 基，工具输出时统一转换为 1 基。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentLogLine {
    /// 文档内 0 基行号。
    pub line_number: usize,
    /// 原始解码正文，不包含行尾换行符。
    pub text: String,
}

/// Agent 专用不可变日志文档；UTF-8 本地文件使用 mmap，其余来源复用安全分页读取器。
#[derive(Clone, Debug)]
pub(crate) struct AgentLogDocument {
    /// UTF-8 本地文件直接映射，其他编码或归档条目按现有阈值选择内存或磁盘分页。
    text: AgentLogText,
    /// mmap 正文中每行的 UTF-8 字节范围；共享读取器分支保持为空。
    line_ranges: Arc<Vec<Range<usize>>>,
    /// 原始文件或解压条目字节数。
    byte_len: u64,
}

/// Agent 日志正文存储；大文件和压缩条目不得退化为无界完整内存副本。
#[derive(Clone, Debug)]
enum AgentLogText {
    /// 本地 UTF-8 文件的只读内存映射和 BOM 后正文起点。
    Utf8Mapped {
        /// 文件只读映射。
        mmap: Arc<Mmap>,
        /// UTF-8 BOM 存在时为 3，否则为 0。
        text_offset: usize,
    },
    /// 复用日志读取层的内存/分页句柄；大型归档会物化到受生命周期管理的临时文件。
    SharedReader(LogReaderHandle),
}

impl AgentLogDocument {
    /// 返回日志原始字节数。
    pub(crate) fn byte_len(&self) -> u64 {
        self.byte_len
    }

    /// 返回日志行数。
    pub(crate) fn line_count(&self) -> usize {
        match &self.text {
            AgentLogText::Utf8Mapped { .. } => self.line_ranges.len(),
            AgentLogText::SharedReader(reader) => reader.line_count(),
        }
    }

    /// 返回指定 0 基行号；mmap 分支零复制，分页分支只解码目标行。
    pub(crate) fn line_text(&self, line_number: usize) -> Option<Cow<'_, str>> {
        match &self.text {
            AgentLogText::Utf8Mapped { mmap, text_offset } => {
                let range = self.line_ranges.get(line_number)?.clone();
                let start = text_offset.checked_add(range.start)?;
                let end = text_offset.checked_add(range.end)?;
                std::str::from_utf8(mmap.get(start..end)?)
                    .ok()
                    .map(Cow::Borrowed)
            }
            AgentLogText::SharedReader(reader) => reader
                .lines(line_number, 1)
                .ok()?
                .into_iter()
                .next()
                .map(|line| Cow::Owned(line.text)),
        }
    }

    /// 读取连续行范围；只有真正返回给工具的行才复制正文。
    pub(crate) fn lines(&self, start_line: usize, max_lines: usize) -> Result<Vec<AgentLogLine>> {
        if max_lines == 0 || start_line >= self.line_count() {
            return Ok(Vec::new());
        }
        match &self.text {
            AgentLogText::Utf8Mapped { .. } => Ok((start_line
                ..start_line.saturating_add(max_lines).min(self.line_count()))
                .filter_map(|line_number| {
                    self.line_text(line_number).map(|text| AgentLogLine {
                        line_number,
                        text: text.into_owned(),
                    })
                })
                .collect()),
            AgentLogText::SharedReader(reader) => Ok(reader
                .lines(start_line, max_lines)?
                .into_iter()
                .map(|line| AgentLogLine {
                    line_number: line.line_number,
                    text: line.text,
                })
                .collect()),
        }
    }

    /// 顺序遍历全部日志行；分页分支合并 I/O，回调返回 `false` 时允许调用方提前停止。
    pub(crate) fn for_each_line(
        &self,
        cancel_flag: &AtomicBool,
        mut callback: impl FnMut(usize, &str) -> bool,
    ) -> Result<bool> {
        match &self.text {
            AgentLogText::Utf8Mapped { .. } => {
                for line_number in 0..self.line_count() {
                    if line_number % 4096 == 0 && cancel_flag.load(Ordering::Relaxed) {
                        bail!("Agent 日志遍历已取消");
                    }
                    let Some(line) = self.line_text(line_number) else {
                        continue;
                    };
                    if !callback(line_number, line.as_ref()) {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            AgentLogText::SharedReader(reader) => {
                let mut was_cancelled = false;
                let completed = reader.for_each_line_in_range(0..reader.line_count(), |line| {
                    if line.line_number % 4096 == 0 && cancel_flag.load(Ordering::Relaxed) {
                        was_cancelled = true;
                        return false;
                    }
                    callback(line.line_number, &line.text)
                })?;
                if was_cancelled {
                    bail!("Agent 日志遍历已取消");
                }
                Ok(completed)
            }
        }
    }

    /// 测试读取器是否进入磁盘分页分支，避免大日志回归为完整内存解码。
    #[cfg(test)]
    fn uses_paged_backend(&self) -> bool {
        matches!(
            &self.text,
            AgentLogText::SharedReader(reader)
                if matches!(reader.document(), crate::log_io::log_file_reader::LogDocument::Paged(_))
        )
    }
}

/// Agent 专用日志文档 LRU；不保存 UI 页签、滚动、最长行或渲染缓存。
#[derive(Debug, Default)]
struct AgentLogDocumentCache {
    /// 队首是最久未使用项。
    entries: VecDeque<(String, AgentLogDocument)>,
}

impl AgentLogDocumentCache {
    /// 判断来源是否已经完成 Agent 原生打开。
    fn contains(&self, source_ref: &str) -> bool {
        self.entries
            .iter()
            .any(|(cached_ref, _)| cached_ref == source_ref)
    }

    /// 获取文档并提升其最近使用顺序。
    fn get(&mut self, source_ref: &str) -> Option<AgentLogDocument> {
        let index = self
            .entries
            .iter()
            .position(|(cached_ref, _)| cached_ref == source_ref)?;
        let entry = self.entries.remove(index)?;
        let document = entry.1.clone();
        self.entries.push_back(entry);
        Some(document)
    }

    /// 插入文档并按固定条目上限淘汰最久未使用项。
    fn insert(&mut self, source_ref: String, document: AgentLogDocument) {
        if let Some(index) = self
            .entries
            .iter()
            .position(|(cached_ref, _)| cached_ref == &source_ref)
        {
            self.entries.remove(index);
        }
        self.entries.push_back((source_ref, document));
        while self.entries.len() > MAX_AGENT_LOG_CACHE_ENTRIES {
            self.entries.pop_front();
        }
    }

    /// 返回缓存条目数量，供回归测试验证共享读取行为。
    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// 不可变的 Agent 来源目录索引；所有路径均为会话展示路径，不包含真实磁盘位置。
#[derive(Debug)]
struct AgentLogCatalog {
    /// 当前会话不可变来源范围。
    scope: Arc<SourceScopeSnapshot>,
    /// 不透明来源引用到 `scope.sources` 下标的 O(1) 索引。
    source_index_by_ref: HashMap<String, usize>,
    /// 文件夹展示路径或精确文件路径到后代来源下标的索引。
    source_indices_by_path: HashMap<String, Vec<usize>>,
    /// 按路径排序的完整目录汇总。
    directories: Vec<AgentLogDirectorySummary>,
}

impl AgentLogCatalog {
    /// 从已固化来源快照一次性构建全部目录索引；之后工具分页不再扫描全来源集合。
    fn new(scope: Arc<SourceScopeSnapshot>) -> Self {
        let mut source_index_by_ref = HashMap::with_capacity(scope.sources.len());
        let mut source_indices_by_path = HashMap::<String, Vec<usize>>::new();
        let mut directory_accumulators = BTreeMap::<String, AgentLogDirectorySummary>::new();

        for (source_index, source) in scope.sources.iter().enumerate() {
            source_index_by_ref.insert(source.source_ref.clone(), source_index);
            source_indices_by_path
                .entry(source.relative_path.clone())
                .or_default()
                .push(source_index);

            let components = source.relative_path.split('/').collect::<Vec<_>>();
            for component_count in 1..components.len() {
                let directory_path = components[..component_count].join("/");
                source_indices_by_path
                    .entry(directory_path.clone())
                    .or_default()
                    .push(source_index);
                let summary = directory_accumulators
                    .entry(directory_path.clone())
                    .or_insert_with(|| AgentLogDirectorySummary {
                        path: directory_path,
                        direct_file_count: 0,
                        descendant_file_count: 0,
                        known_total_bytes: 0,
                    });
                summary.descendant_file_count = summary.descendant_file_count.saturating_add(1);
                summary.known_total_bytes = summary
                    .known_total_bytes
                    .saturating_add(source.size.unwrap_or_default());
                if component_count + 1 == components.len() {
                    summary.direct_file_count = summary.direct_file_count.saturating_add(1);
                }
            }
        }

        Self {
            scope,
            source_index_by_ref,
            source_indices_by_path,
            directories: directory_accumulators.into_values().collect(),
        }
    }

    /// 按不透明引用返回授权来源。
    fn source(&self, source_ref: &str) -> Option<&SnapshotSource> {
        self.source_index_by_ref
            .get(source_ref)
            .and_then(|index| self.scope.sources.get(*index))
    }

    /// 返回可选路径前缀内的来源分页及精确总数。
    fn source_page(
        &self,
        path_prefix: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> (Vec<SnapshotSource>, usize) {
        match path_prefix {
            Some(prefix) => {
                let Some(source_indices) = self.source_indices_by_path.get(prefix) else {
                    return (Vec::new(), 0);
                };
                let page = source_indices
                    .iter()
                    .skip(offset)
                    .take(limit)
                    .filter_map(|index| self.scope.sources.get(*index).cloned())
                    .collect();
                (page, source_indices.len())
            }
            None => {
                let page = self
                    .scope
                    .sources
                    .iter()
                    .skip(offset)
                    .take(limit)
                    .cloned()
                    .collect();
                (page, self.scope.sources.len())
            }
        }
    }

    /// 生成完整、可分页保存的 TSV 目录清单；控制字符会被替换，防止破坏记录边界。
    fn manifest(&self) -> String {
        let mut manifest = String::with_capacity(self.scope.sources.len().saturating_mul(96));
        manifest.push_str("kind\tpath\tsource_ref\tsize\tprofile_id\tdescendant_files\n");
        for directory in &self.directories {
            manifest.push_str("directory\t");
            manifest.push_str(&sanitize_manifest_field(&directory.path));
            manifest.push_str("\t\t\t\t");
            manifest.push_str(&directory.descendant_file_count.to_string());
            manifest.push('\n');
        }
        for source in self.scope.sources.iter() {
            manifest.push_str("file\t");
            manifest.push_str(&sanitize_manifest_field(&source.relative_path));
            manifest.push('\t');
            manifest.push_str(&source.source_ref);
            manifest.push('\t');
            if let Some(size) = source.size {
                manifest.push_str(&size.to_string());
            }
            manifest.push('\t');
            if let Some(profile_id) = source.profile_id.as_deref() {
                manifest.push_str(&sanitize_manifest_field(profile_id));
            }
            manifest.push_str("\t\n");
        }
        manifest
    }
}

/// Agent 会话级日志访问服务；克隆只复制 `Arc`，不会复制目录或日志正文。
#[derive(Clone, Debug)]
pub(crate) struct AgentLogAccess {
    inner: Arc<AgentLogAccessInner>,
}

/// 访问服务共享状态；读取器缓存和打开门闩只在当前会话内存中存在。
#[derive(Debug)]
struct AgentLogAccessInner {
    /// 不可变快速目录索引。
    catalog: AgentLogCatalog,
    /// 已完成解压、编码检测和行索引的读取器 LRU。
    reader_cache: Mutex<AgentLogDocumentCache>,
    /// 同一来源的并发打开门闩，避免多个工具同时重复解压或建立索引。
    open_gates: Mutex<HashMap<String, Weak<Mutex<()>>>>,
    /// 首次请求时生成的完整目录清单；后续工具调用直接复用，避免反复拼接大量路径。
    manifest: OnceLock<AgentLogManifest>,
}

/// 会话内缓存的完整目录清单及字符数，避免每次返回元数据时重复遍历正文。
#[derive(Debug)]
struct AgentLogManifest {
    /// TSV 清单正文。
    content: String,
    /// 模型分页工具使用的 Unicode 字符数量。
    character_count: usize,
}

impl AgentLogAccess {
    /// 为一个不可变来源范围创建独立 Agent 访问服务。
    pub(crate) fn new(scope: Arc<SourceScopeSnapshot>) -> Self {
        Self {
            inner: Arc::new(AgentLogAccessInner {
                catalog: AgentLogCatalog::new(scope),
                reader_cache: Mutex::new(AgentLogDocumentCache::default()),
                open_gates: Mutex::new(HashMap::new()),
                manifest: OnceLock::new(),
            }),
        }
    }

    /// O(1) 解析不透明来源引用。
    pub(crate) fn source(&self, source_ref: &str) -> Option<&SnapshotSource> {
        self.inner.catalog.source(source_ref)
    }

    /// 从预构建路径索引返回一页来源，目录包含所有日志后代，文件路径只返回自身。
    pub(crate) fn source_page(
        &self,
        path_prefix: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> (Vec<SnapshotSource>, usize) {
        self.inner.catalog.source_page(path_prefix, offset, limit)
    }

    /// 返回按路径排序的完整目录汇总。
    pub(crate) fn directories(&self) -> &[AgentLogDirectorySummary] {
        &self.inner.catalog.directories
    }

    /// 返回包含全部目录、文件和 source_ref 的本地会话清单；内容在首次请求后缓存。
    pub(crate) fn manifest(&self) -> &str {
        &self
            .inner
            .manifest
            .get_or_init(|| {
                let content = self.inner.catalog.manifest();
                let character_count = content.chars().count();
                AgentLogManifest {
                    content,
                    character_count,
                }
            })
            .content
    }

    /// 返回缓存目录清单的字符数，不为分页元数据重复遍历完整字符串。
    pub(crate) fn manifest_character_count(&self) -> usize {
        // 先调用 `manifest` 保证同一个 `OnceLock` 已完成初始化，再读取同一不可变记录。
        let _ = self.manifest();
        self.inner
            .manifest
            .get()
            .map_or(0, |manifest| manifest.character_count)
    }

    /// 判断来源是否已经完成打开，供独立复核在工具入口区分有限缓存读取和重新扫描。
    pub(crate) fn has_cached_reader(&self, source_ref: &str) -> Result<bool> {
        self.inner
            .reader_cache
            .lock()
            .map(|cache| cache.contains(source_ref))
            .map_err(|_| anyhow!("Agent 日志读取器缓存状态已损坏"))
    }

    /// 打开一个授权日志；缓存命中时不重复解压和索引，并合并同源并发首次打开。
    pub(crate) fn open(
        &self,
        source_ref: &str,
        cancel_flag: Arc<AtomicBool>,
    ) -> Result<AgentOpenedLog> {
        if let Some(reader) = self
            .inner
            .reader_cache
            .lock()
            .map_err(|_| anyhow!("Agent 日志读取器缓存状态已损坏"))?
            .get(source_ref)
        {
            return Ok(AgentOpenedLog {
                reader,
                cache_hit: true,
            });
        }
        let source = self
            .source(source_ref)
            .cloned()
            .ok_or_else(|| anyhow!("source_ref 不在当前 Agent 会话范围内"))?;
        let gate = {
            let mut gates = self
                .inner
                .open_gates
                .lock()
                .map_err(|_| anyhow!("Agent 日志打开状态已损坏"))?;
            gates
                .get(source_ref)
                .and_then(Weak::upgrade)
                .unwrap_or_else(|| {
                    let gate = Arc::new(Mutex::new(()));
                    gates.insert(source_ref.to_string(), Arc::downgrade(&gate));
                    gate
                })
        };
        let _open_guard = gate
            .lock()
            .map_err(|_| anyhow!("Agent 日志打开任务状态已损坏"))?;
        if let Some(reader) = self
            .inner
            .reader_cache
            .lock()
            .map_err(|_| anyhow!("Agent 日志读取器缓存状态已损坏"))?
            .get(source_ref)
        {
            return Ok(AgentOpenedLog {
                reader,
                cache_hit: true,
            });
        }
        let reader = open_agent_log_document(&source, &self.inner.catalog.scope, cancel_flag)?;
        self.inner
            .reader_cache
            .lock()
            .map_err(|_| anyhow!("Agent 日志读取器缓存状态已损坏"))?
            .insert(source_ref.to_string(), reader.clone());
        Ok(AgentOpenedLog {
            reader,
            cache_hit: false,
        })
    }

    /// 绕过会话缓存重新读取当前来源，供证据校验检测日志轮转、截断或同规模覆盖。
    pub(crate) fn open_fresh(
        &self,
        source_ref: &str,
        cancel_flag: Arc<AtomicBool>,
    ) -> Result<AgentLogDocument> {
        let source = self
            .source(source_ref)
            .ok_or_else(|| anyhow!("source_ref 不在当前 Agent 会话范围内"))?;
        open_agent_log_document(source, &self.inner.catalog.scope, cancel_flag)
    }

    /// 返回当前缓存读取器数量，供性能回归测试确认工具确实共享打开结果。
    #[cfg(test)]
    pub(crate) fn cached_reader_count(&self) -> usize {
        self.inner
            .reader_cache
            .lock()
            .map(|cache| cache.len())
            .unwrap_or_default()
    }
}

/// 打开 Agent 日志；本地 UTF-8 文件优先 mmap，其余来源复用统一的安全分页读取后端。
fn open_agent_log_document(
    source: &SnapshotSource,
    scope: &SourceScopeSnapshot,
    cancel_flag: Arc<AtomicBool>,
) -> Result<AgentLogDocument> {
    if let SourceLocation::LocalPath(path) = &source.location
        && let Some(document) = try_open_agent_utf8_mmap(path, cancel_flag.as_ref())?
    {
        return Ok(document);
    }
    if cancel_flag.load(Ordering::Relaxed) {
        bail!("Agent 日志读取已取消");
    }
    // 归档条目、非 UTF-8 文件及 mmap 失败的本地文件统一走现有读取器：超过阈值时自动落盘并分页，
    // 避免 Agent 为完整原始字节和解码字符串同时分配内存。
    let reader = LogFileReader::open_with_cancel_flag(
        OpenLogRequest {
            location: source.location.clone(),
            label: source.file_name.clone(),
            default_encoding: scope.default_encoding.clone(),
        },
        cancel_flag,
    )?;
    let byte_len = reader.byte_len();
    Ok(AgentLogDocument {
        text: AgentLogText::SharedReader(reader),
        line_ranges: Arc::new(Vec::new()),
        byte_len,
    })
}

/// 尝试把本地 UTF-8 日志直接映射为 Agent 文档；非 UTF-8 或映射失败时交给通用解码路径。
fn try_open_agent_utf8_mmap(
    path: &std::path::Path,
    cancel_flag: &AtomicBool,
) -> Result<Option<AgentLogDocument>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return Ok(None),
    };
    let byte_len = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    if byte_len == 0 {
        // 空文件无法 mmap，交给统一读取器创建轻量内存文档。
        return Ok(None);
    }
    // SAFETY：映射只读文件且 `Mmap` 与文件内容生命周期独立；所有切片仍经过 UTF-8 校验和范围检查。
    let mmap = match unsafe { MmapOptions::new().map(&file) } {
        Ok(mmap) => mmap,
        Err(_) => return Ok(None),
    };
    let text_offset = usize::from(mmap.starts_with(&[0xEF, 0xBB, 0xBF])) * 3;
    let Some(text_bytes) = mmap.get(text_offset..) else {
        return Ok(None);
    };
    let Ok(text) = std::str::from_utf8(text_bytes) else {
        return Ok(None);
    };
    let line_ranges = build_agent_line_ranges(text, cancel_flag)?;
    Ok(Some(AgentLogDocument {
        text: AgentLogText::Utf8Mapped {
            mmap: Arc::new(mmap),
            text_offset,
        },
        line_ranges: Arc::new(line_ranges),
        byte_len,
    }))
}

/// 为 Agent 文档建立紧凑 UTF-8 行范围；尾部换行不会额外生成不存在的空行。
fn build_agent_line_ranges(text: &str, cancel_flag: &AtomicBool) -> Result<Vec<Range<usize>>> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let bytes = text.as_bytes();
    let mut ranges = Vec::new();
    let mut line_start = 0_usize;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if index % AGENT_FILE_READ_CHUNK_BYTES == 0 && cancel_flag.load(Ordering::Relaxed) {
            bail!("Agent 日志行索引已取消");
        }
        if byte != b'\n' {
            continue;
        }
        let line_end = if index > line_start && bytes[index - 1] == b'\r' {
            index - 1
        } else {
            index
        };
        ranges.push(line_start..line_end);
        line_start = index + 1;
    }
    if line_start < bytes.len() {
        let line_end = if bytes.last() == Some(&b'\r') {
            bytes.len() - 1
        } else {
            bytes.len()
        };
        ranges.push(line_start..line_end);
    }
    Ok(ranges)
}

/// 清理 TSV 字段中的换行和制表符，保证模型分页读取时记录边界稳定。
fn sanitize_manifest_field(value: &str) -> String {
    value.replace(['\t', '\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::config::paths::temporary_test_dir;
    use crate::loader::SourceId;
    use crate::loader::SourceLocation;
    use crate::log_io::log_file_reader::LARGE_LOG_THRESHOLD_BYTES;

    /// 用指定来源构造最小不可变范围，确保测试配置和临时文件不会访问生产目录。
    fn test_scope(sources: Vec<SnapshotSource>) -> Arc<SourceScopeSnapshot> {
        Arc::new(SourceScopeSnapshot {
            session_id: "agent-log-access-test".to_string(),
            root_label: "全部测试来源".to_string(),
            sources: Arc::new(sources),
            profiles: Arc::new(HashMap::new()),
            default_encoding: "UTF-8".to_string(),
            allow_raw_log_content: true,
        })
    }

    /// 构造只用于目录索引测试的来源；目录测试不会实际打开占位路径。
    fn indexed_source(source_id: usize, source_ref: &str, relative_path: &str) -> SnapshotSource {
        SnapshotSource {
            source_ref: source_ref.to_string(),
            source_id: SourceId(source_id),
            file_name: relative_path
                .rsplit('/')
                .next()
                .unwrap_or(relative_path)
                .to_string(),
            relative_path: relative_path.to_string(),
            profile_match_path: relative_path.to_string(),
            location: SourceLocation::LocalPath(PathBuf::from(relative_path)),
            size: Some(10),
            profile_id: None,
        }
    }

    /// 验证预构建目录索引按完整路径段筛选，不会把同字符串前缀目录混入结果。
    #[test]
    fn catalog_filters_descendants_by_complete_path_segment() {
        let access = AgentLogAccess::new(test_scope(vec![
            indexed_source(1, "app-source", "来源/logs/app/server.log"),
            indexed_source(2, "application-source", "来源/logs/application/server.log"),
        ]));

        let (sources, total) = access.source_page(Some("来源/logs/app"), 0, 100);

        assert_eq!(total, 1);
        assert_eq!(sources[0].source_ref, "app-source");
        assert_eq!(
            access
                .source("application-source")
                .map(|source| source.source_id),
            Some(SourceId(2))
        );
    }

    /// 验证一次生成的完整目录制品包含目录、文件和模型后续读取所需的不透明引用。
    #[test]
    fn manifest_contains_complete_directory_and_file_records() {
        let access = AgentLogAccess::new(test_scope(vec![
            indexed_source(1, "first-source", "来源/logs/app.log"),
            indexed_source(2, "second-source", "来源/gc/gc.log"),
        ]));

        let manifest = access.manifest();

        assert!(manifest.contains("directory\t来源/logs"));
        assert!(manifest.contains("directory\t来源/gc"));
        assert!(manifest.contains("file\t来源/logs/app.log\tfirst-source"));
        assert!(manifest.contains("file\t来源/gc/gc.log\tsecond-source"));
    }

    /// 验证不同工具重复读取同一日志时直接复用已完成编码检测和行索引的共享句柄。
    #[test]
    fn repeated_open_reuses_session_reader() {
        let directory = temporary_test_dir("agent-log-access-cache");
        let path = directory.path().join("application.log");
        fs::write(&path, "first\nsecond\n").expect("应写入 Agent 测试日志");
        let scope = test_scope(vec![SnapshotSource {
            source_ref: "cached-source".to_string(),
            source_id: SourceId(1),
            file_name: "application.log".to_string(),
            relative_path: "来源/application.log".to_string(),
            profile_match_path: "application.log".to_string(),
            location: SourceLocation::LocalPath(path),
            size: Some(13),
            profile_id: None,
        }]);
        let access = AgentLogAccess::new(scope);

        let first = access
            .open("cached-source", Arc::new(AtomicBool::new(false)))
            .expect("首次打开日志应成功");
        let second = access
            .open("cached-source", Arc::new(AtomicBool::new(false)))
            .expect("重复打开日志应成功");

        assert!(!first.cache_hit);
        assert!(second.cache_hit);
        assert_eq!(second.reader.line_count(), 2);
        assert_eq!(access.cached_reader_count(), 1);
    }

    /// 验证大型非 UTF-8 本地日志交给磁盘分页后端，不再分配完整原始字节和解码字符串。
    #[test]
    fn large_non_utf8_log_uses_paged_backend() {
        let directory = temporary_test_dir("agent-log-access-large-non-utf8");
        let path = directory.path().join("legacy.log");
        let mut file = File::create(&path).expect("应创建大型非 UTF-8 测试日志");
        file.write_all(&[0xFF])
            .expect("应写入可触发非 UTF-8 分支的文件头");
        file.set_len(LARGE_LOG_THRESHOLD_BYTES + 1)
            .expect("应以稀疏文件方式扩展到分页阈值以上");
        drop(file);
        let scope = test_scope(vec![SnapshotSource {
            source_ref: "large-legacy-source".to_string(),
            source_id: SourceId(1),
            file_name: "legacy.log".to_string(),
            relative_path: "来源/legacy.log".to_string(),
            profile_match_path: "legacy.log".to_string(),
            location: SourceLocation::LocalPath(path),
            size: Some(LARGE_LOG_THRESHOLD_BYTES + 1),
            profile_id: None,
        }]);
        let access = AgentLogAccess::new(scope);

        let opened = access
            .open("large-legacy-source", Arc::new(AtomicBool::new(false)))
            .expect("大型非 UTF-8 日志应成功建立分页索引");

        assert!(opened.reader.uses_paged_backend());
        assert_eq!(opened.reader.byte_len(), LARGE_LOG_THRESHOLD_BYTES + 1);
    }
}
