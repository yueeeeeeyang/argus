//! 文件职责：定义压缩包统一适配器抽象。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：为 ZIP、TAR、压缩 TAR、7Z 和 RAR 等格式提供统一识别、枚举、读取和能力声明模型。

use std::io::{Cursor, Read, Seek};
use std::path::Path;

use anyhow::{Context as _, Result};

use crate::loader::archive::detector::ArchiveFormat;
use crate::loader::archive::registry::archive_registry;
use crate::utils::path::normalize_archive_entry_path;

/// 压缩包条目枚举结果；只保存结构信息，不读取日志正文内容。
#[derive(Clone, Debug)]
pub struct ArchiveEntryInfo {
    /// 压缩包内规范化路径，统一使用 `/` 分隔。
    pub path: String,
    /// 条目名称，用于来源树显示。
    pub label: String,
    /// 是否为目录条目。
    pub is_dir: bool,
    /// 条目未压缩大小；部分格式可能无法提供。
    pub size: Option<u64>,
}

/// 压缩包根层单文件探测结果。
#[derive(Clone, Debug)]
pub enum ArchiveRootProbe {
    /// 根层恰好只有一个普通文件，可被折叠为可直接打开的来源树叶子。
    SingleFile(ArchiveEntryInfo),
    /// 根层恰好只有一个文件，但该文件本身仍是压缩包；当前容器保持可展开。
    SingleNestedArchive {
        /// 唯一根层文件条目。
        entry: ArchiveEntryInfo,
        /// 唯一根层文件的压缩格式。
        format: ArchiveFormat,
    },
    /// 根层为空、包含目录、包含多个文件，或唯一文件位于子目录中。
    NotSingle,
}

/// 根层单文件短路探测状态机，供各格式适配器复用。
#[derive(Debug, Default)]
pub struct ArchiveRootProbeState {
    /// 当前唯一候选文件；一旦发现第二个候选或目录即判定不是单文件压缩包。
    candidate: Option<ArchiveEntryInfo>,
    /// 是否已经判定不是根层单文件。
    is_not_single: bool,
}

impl ArchiveRootProbeState {
    /// 记录一个条目；返回 `false` 表示已经可以短路停止枚举。
    pub fn observe(&mut self, mut entry: ArchiveEntryInfo) -> bool {
        if self.is_not_single {
            return false;
        }

        entry.path = normalize_archive_entry_path(&entry.path);
        if entry.path.is_empty() {
            return true;
        }

        let mut parts = entry.path.split('/').filter(|part| !part.is_empty());
        let Some(_) = parts.next() else {
            return true;
        };
        if entry.is_dir || parts.next().is_some() || self.candidate.is_some() {
            self.is_not_single = true;
            self.candidate = None;
            return false;
        }

        self.candidate = Some(entry);
        true
    }

    /// 根据已观察条目生成最终探测结果。
    pub fn finish(self) -> ArchiveRootProbe {
        if self.is_not_single {
            return ArchiveRootProbe::NotSingle;
        }

        let Some(entry) = self.candidate else {
            return ArchiveRootProbe::NotSingle;
        };

        match archive_registry().detect_name(&entry.path) {
            Some(format) => ArchiveRootProbe::SingleNestedArchive { entry, format },
            None => ArchiveRootProbe::SingleFile(entry),
        }
    }
}

/// 压缩格式能力声明，供 UI、加载器和后续格式扩展判断可用能力。
#[derive(Clone, Copy, Debug)]
pub struct ArchiveCapabilities {
    /// 该能力声明对应的压缩格式。
    pub format: ArchiveFormat,
    /// 面向用户展示的格式名称。
    pub label: &'static str,
    /// 可通过文件名识别的扩展名列表，包含前导点并按完整扩展名书写。
    pub extensions: &'static [&'static str],
    /// 是否支持通过文件头识别格式。
    pub supports_header_detection: bool,
    /// 是否支持枚举压缩包条目。
    pub supports_listing: bool,
    /// 是否支持读取单个条目字节。
    pub supports_entry_reading: bool,
    /// 是否支持作为嵌套压缩包继续展开。
    pub supports_nested_archives: bool,
    /// 是否支持密码或加密压缩包；当前内置适配器均暂不提供密码输入。
    pub supports_passwords: bool,
}

/// 任意可读且可定位的压缩数据源；用于对象安全地把内存压缩包交给适配器。
pub trait ArchiveReadSeek: Read + Seek {}

impl<T> ArchiveReadSeek for T where T: Read + Seek {}

/// 压缩包条目流式输出回调；适配器每读取到一段解压后字节就调用一次。
pub type ArchiveEntryConsumer<'a> = dyn FnMut(&[u8]) -> Result<()> + 'a;

/// 压缩包适配器统一接口；每个格式自行声明识别规则、能力和读写入口。
pub trait ArchiveAdapter: Sync {
    /// 返回当前适配器的能力声明。
    fn capabilities(&self) -> ArchiveCapabilities;

    /// 判断文件头样本是否匹配当前压缩格式。
    fn matches_header(&self, _sample: &[u8]) -> bool {
        false
    }

    /// 判断已转为小写的文件名是否匹配当前格式扩展名。
    fn matches_name(&self, lowercase_name: &str) -> bool {
        self.capabilities()
            .extensions
            .iter()
            .any(|extension| lowercase_name.ends_with(extension))
    }

    /// 枚举本地压缩包条目。
    ///
    /// 参数说明：
    /// - `path`：本地压缩包路径。
    ///
    /// 返回值：压缩包内条目列表；不执行正文读取或解压到磁盘。
    fn list_entries(&self, path: &Path) -> Result<Vec<ArchiveEntryInfo>>;

    /// 枚举内存或其他可 seek 数据源中的压缩包条目。
    ///
    /// 参数说明：
    /// - `reader`：压缩包数据源。
    /// - `reader_len`：数据源总长度，供 7Z 等需要长度的格式使用。
    /// - `source_label`：错误消息中的虚拟来源名称。
    fn list_entries_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        reader_len: u64,
        source_label: &str,
    ) -> Result<Vec<ArchiveEntryInfo>>;

    /// 轻量探测根层是否恰好只有一个普通文件。
    ///
    /// 默认实现复用完整枚举，保证新增格式无需立即实现短路探测也能保持正确性；
    /// 核心内置格式会覆盖该方法，只枚举到足以判定的条目后立即返回。
    fn probe_single_file_root(&self, path: &Path) -> Result<ArchiveRootProbe> {
        Ok(probe_single_file_root_from_entries(
            self.list_entries(path)?,
        ))
    }

    /// 对内存或其他可 seek 数据源做根层单文件轻量探测。
    fn probe_single_file_root_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        reader_len: u64,
        source_label: &str,
    ) -> Result<ArchiveRootProbe> {
        Ok(probe_single_file_root_from_entries(
            self.list_entries_from_reader(reader, reader_len, source_label)?,
        ))
    }

    /// 从本地压缩包读取指定条目的完整字节。
    ///
    /// 返回值：目标条目原始字节；用于嵌套压缩包继续解析。
    fn read_entry_bytes(&self, path: &Path, entry_path: &str) -> Result<Vec<u8>>;

    /// 从内存或其他可 seek 数据源读取指定条目的完整字节。
    fn read_entry_bytes_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        reader_len: u64,
        entry_path: &str,
        source_label: &str,
    ) -> Result<Vec<u8>>;

    /// 从本地压缩包流式输出指定条目内容。
    ///
    /// 默认实现会复用完整字节读取能力，保证新增格式只实现旧接口也能工作；
    /// ZIP、TAR、压缩 TAR、7Z 等内置适配器会覆盖为真正的 chunk 回调。
    fn stream_entry(
        &self,
        path: &Path,
        entry_path: &str,
        consumer: &mut ArchiveEntryConsumer<'_>,
    ) -> Result<()> {
        let bytes = self.read_entry_bytes(path, entry_path)?;
        consumer(&bytes)
    }

    /// 从内存或其他可 seek 数据源流式输出指定条目内容。
    fn stream_entry_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        reader_len: u64,
        entry_path: &str,
        source_label: &str,
        consumer: &mut ArchiveEntryConsumer<'_>,
    ) -> Result<()> {
        let bytes =
            self.read_entry_bytes_from_reader(reader, reader_len, entry_path, source_label)?;
        consumer(&bytes)
    }
}

/// 使用完整条目列表兜底推导根层单文件探测结果。
pub fn probe_single_file_root_from_entries(entries: Vec<ArchiveEntryInfo>) -> ArchiveRootProbe {
    let mut state = ArchiveRootProbeState::default();
    for entry in entries {
        if !state.observe(entry) {
            break;
        }
    }
    state.finish()
}

/// 根据注册表中的压缩适配器枚举压缩包条目。
pub fn list_archive_entries(path: &Path, format: ArchiveFormat) -> Result<Vec<ArchiveEntryInfo>> {
    archive_registry().list_entries(format, path)
}

/// 从内存字节枚举压缩包条目。
///
/// 参数说明：
/// - `bytes`：压缩包完整字节，通常来自父 ZIP 内的内嵌 ZIP 条目。
/// - `format`：当前内存压缩包格式。
/// - `source_label`：错误提示中的虚拟来源名称。
///
/// 返回值：压缩包内条目列表；用于从父压缩包读取出的内嵌容器。
pub fn list_archive_entries_from_bytes(
    bytes: Vec<u8>,
    format: ArchiveFormat,
    source_label: &str,
) -> Result<Vec<ArchiveEntryInfo>> {
    list_archive_entries_from_reader(Cursor::new(bytes), format, source_label)
}

/// 从任意可读可 seek 的输入枚举压缩包条目。
///
/// 参数说明：
/// - `reader`：压缩包数据来源，可为本地文件或内存 Cursor。
/// - `format`：当前压缩包格式。
/// - `source_label`：错误提示中的来源名称。
///
/// 返回值：压缩包内条目列表；用于嵌套压缩包的统一内存枚举。
pub fn list_archive_entries_from_reader<R>(
    mut reader: R,
    format: ArchiveFormat,
    source_label: &str,
) -> Result<Vec<ArchiveEntryInfo>>
where
    R: Read + Seek,
{
    let reader_len = reader
        .seek(std::io::SeekFrom::End(0))
        .with_context(|| format!("无法读取压缩包大小：{source_label}"))?;
    reader
        .seek(std::io::SeekFrom::Start(0))
        .with_context(|| format!("无法重置压缩包读取位置：{source_label}"))?;

    archive_registry().list_entries_from_reader(format, &mut reader, reader_len, source_label)
}

/// 从本地压缩包及其嵌套容器链路读取指定条目的完整字节。
///
/// 参数说明：
/// - `archive_path`：最外层真实压缩包路径。
/// - `root_format`：最外层压缩包格式。
/// - `container_entries`：从外层到当前容器之间的嵌套压缩包条目链路。
/// - `entry_path`：需要从当前容器中读取的条目路径。
///
/// 返回值：条目完整字节；调用方可继续将其作为下一层压缩包解析。
pub fn read_archive_entry_bytes(
    archive_path: &Path,
    root_format: ArchiveFormat,
    container_entries: &[String],
    entry_path: &str,
) -> Result<Vec<u8>> {
    if container_entries.is_empty() {
        return archive_registry().read_entry_bytes(root_format, archive_path, entry_path);
    }

    let first_container = &container_entries[0];
    let mut current_format = root_format;
    let mut bytes =
        archive_registry().read_entry_bytes(current_format, archive_path, first_container)?;
    let mut current_label = format!("{}!/{first_container}", archive_path.display());
    current_format = detect_container_format(first_container)?;

    for container_entry in &container_entries[1..] {
        bytes = read_archive_entry_bytes_from_reader(
            Cursor::new(bytes),
            current_format,
            container_entry,
            &current_label,
        )?;
        current_label.push_str("!/");
        current_label.push_str(container_entry);
        current_format = detect_container_format(container_entry)?;
    }

    read_archive_entry_bytes_from_reader(
        Cursor::new(bytes),
        current_format,
        entry_path,
        &current_label,
    )
}

/// 从本地压缩包及其嵌套容器链路流式读取目标日志条目。
///
/// 参数说明：
/// - `archive_path`：最外层真实压缩包路径。
/// - `root_format`：最外层压缩包格式。
/// - `container_entries`：内嵌容器链路，必要时短期在内存中持有中间容器字节。
/// - `entry_path`：最终需要读取的日志条目路径。
/// - `consumer`：接收解压后字节分片的回调。
///
/// 返回值：读取成功返回 `Ok(())`；正文读取不创建临时文件。
pub fn stream_archive_entry(
    archive_path: &Path,
    root_format: ArchiveFormat,
    container_entries: &[String],
    entry_path: &str,
    consumer: &mut ArchiveEntryConsumer<'_>,
) -> Result<()> {
    if container_entries.is_empty() {
        return archive_registry().stream_entry(root_format, archive_path, entry_path, consumer);
    }

    let first_container = &container_entries[0];
    let mut current_format = root_format;
    let mut bytes =
        archive_registry().read_entry_bytes(current_format, archive_path, first_container)?;
    let mut current_label = format!("{}!/{first_container}", archive_path.display());
    current_format = detect_container_format(first_container)?;

    for container_entry in &container_entries[1..] {
        bytes = read_archive_entry_bytes_from_reader(
            Cursor::new(bytes),
            current_format,
            container_entry,
            &current_label,
        )?;
        current_label.push_str("!/");
        current_label.push_str(container_entry);
        current_format = detect_container_format(container_entry)?;
    }

    stream_archive_entry_from_reader(
        Cursor::new(bytes),
        current_format,
        entry_path,
        &current_label,
        consumer,
    )
}

/// 从任意压缩包数据源读取指定条目字节。
fn read_archive_entry_bytes_from_reader<R>(
    mut reader: R,
    format: ArchiveFormat,
    entry_path: &str,
    source_label: &str,
) -> Result<Vec<u8>>
where
    R: Read + Seek,
{
    let reader_len = reader
        .seek(std::io::SeekFrom::End(0))
        .with_context(|| format!("无法读取压缩包大小：{source_label}"))?;
    reader
        .seek(std::io::SeekFrom::Start(0))
        .with_context(|| format!("无法重置压缩包读取位置：{source_label}"))?;

    archive_registry().read_entry_bytes_from_reader(
        format,
        &mut reader,
        reader_len,
        entry_path,
        source_label,
    )
}

/// 从任意压缩包数据源流式读取指定条目字节。
fn stream_archive_entry_from_reader<R>(
    mut reader: R,
    format: ArchiveFormat,
    entry_path: &str,
    source_label: &str,
    consumer: &mut ArchiveEntryConsumer<'_>,
) -> Result<()>
where
    R: Read + Seek,
{
    let reader_len = reader
        .seek(std::io::SeekFrom::End(0))
        .with_context(|| format!("无法读取压缩包大小：{source_label}"))?;
    reader
        .seek(std::io::SeekFrom::Start(0))
        .with_context(|| format!("无法重置压缩包读取位置：{source_label}"))?;

    archive_registry().stream_entry_from_reader(
        format,
        &mut reader,
        reader_len,
        entry_path,
        source_label,
        consumer,
    )
}

/// 根据容器条目名称推导下一层压缩格式。
fn detect_container_format(entry_path: &str) -> Result<ArchiveFormat> {
    archive_registry()
        .detect_name(entry_path)
        .with_context(|| format!("无法识别嵌套压缩包格式：{entry_path}"))
}
