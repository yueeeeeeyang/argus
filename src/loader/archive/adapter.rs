//! 文件职责：定义压缩包统一适配器抽象。
//! 创建日期：2026-06-09
//! 修改日期：2026-07-17
//! 作者：Argus 开发团队
//! 主要功能：为 ZIP、TAR、压缩 TAR、7Z 和 RAR 等格式提供统一识别、枚举、单条读取、批量访问和能力声明模型。

use std::collections::HashSet;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context as _, Result};

use crate::loader::archive::detector::ArchiveFormat;
use crate::loader::archive::password::{ArchivePasswordKey, ArchivePasswordStore};
use crate::loader::archive::registry::archive_registry;

/// 压缩包条目枚举结果；只保存结构信息，不读取日志正文内容。
#[derive(Clone, Debug)]
pub(crate) struct ArchiveEntryInfo {
    /// 压缩包内规范化路径，统一使用 `/` 分隔。
    pub path: String,
    /// 是否为目录条目。
    pub is_dir: bool,
    /// 条目未压缩大小；部分格式可能无法提供。
    pub size: Option<u64>,
}

/// 压缩格式能力声明，供 UI、加载器和后续格式扩展判断可用能力。
#[derive(Clone, Copy, Debug)]
pub(crate) struct ArchiveCapabilities {
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
}

/// 任意可读且可定位的压缩数据源；用于对象安全地把内存压缩包交给适配器。
pub(crate) trait ArchiveReadSeek: Read + Seek {}

impl<T> ArchiveReadSeek for T where T: Read + Seek {}

/// 压缩包条目流式输出回调；适配器每读取到一段解压后字节就调用一次。
pub(crate) type ArchiveEntryConsumer<'a> = dyn FnMut(&[u8]) -> Result<()> + 'a;

/// 多条归档日志的顺序访问回调；读取器只在回调期间有效，调用方必须当场消费完目标条目。
pub(crate) type ArchiveEntriesConsumer<'a> = dyn FnMut(&str, &mut dyn Read) -> Result<()> + 'a;

/// 压缩包适配器统一接口；每个格式自行声明识别规则、能力和读写入口。
pub(crate) trait ArchiveAdapter: Sync {
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
    fn list_entries(&self, path: &Path, password: Option<&str>) -> Result<Vec<ArchiveEntryInfo>>;

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
        password: Option<&str>,
    ) -> Result<Vec<ArchiveEntryInfo>>;

    /// 从本地压缩包读取指定条目的完整字节。
    ///
    /// 返回值：目标条目原始字节；用于嵌套压缩包继续解析。
    fn read_entry_bytes(
        &self,
        path: &Path,
        entry_path: &str,
        password: Option<&str>,
    ) -> Result<Vec<u8>>;

    /// 从内存或其他可 seek 数据源读取指定条目的完整字节。
    fn read_entry_bytes_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        reader_len: u64,
        entry_path: &str,
        source_label: &str,
        password: Option<&str>,
    ) -> Result<Vec<u8>>;

    /// 从本地压缩包流式输出指定条目内容。
    ///
    /// 默认实现会复用完整字节读取能力，保证新增格式只实现旧接口也能工作；
    /// ZIP、TAR、压缩 TAR、7Z 等内置适配器会覆盖为真正的 chunk 回调。
    fn stream_entry(
        &self,
        path: &Path,
        entry_path: &str,
        password: Option<&str>,
        consumer: &mut ArchiveEntryConsumer<'_>,
    ) -> Result<()> {
        let bytes = self.read_entry_bytes(path, entry_path, password)?;
        consumer(&bytes)
    }

    /// 从内存或其他可 seek 数据源流式输出指定条目内容。
    fn stream_entry_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        reader_len: u64,
        entry_path: &str,
        source_label: &str,
        password: Option<&str>,
        consumer: &mut ArchiveEntryConsumer<'_>,
    ) -> Result<()> {
        let bytes = self.read_entry_bytes_from_reader(
            reader,
            reader_len,
            entry_path,
            source_label,
            password,
        )?;
        consumer(&bytes)
    }

    /// 打开一次本地物理容器，并顺序访问其中所有目标条目。
    ///
    /// 默认实现用于兼容不具备批量解包能力的扩展格式；ZIP、TAR、压缩 TAR、GZIP 和 7Z
    /// 均覆盖此方法，保证一个物理容器只解析一次。
    fn visit_entries(
        &self,
        path: &Path,
        entry_paths: &HashSet<String>,
        password: Option<&str>,
        consumer: &mut ArchiveEntriesConsumer<'_>,
    ) -> Result<()> {
        for entry_path in entry_paths {
            let bytes = self.read_entry_bytes(path, entry_path, password)?;
            consumer(entry_path, &mut Cursor::new(bytes))?;
        }
        Ok(())
    }

    /// 在一个已经物化的嵌套容器上顺序访问全部目标条目。
    ///
    /// 默认实现会在每个兼容读取前把输入复位；核心格式均覆盖为单次解析实现。
    fn visit_entries_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        reader_len: u64,
        entry_paths: &HashSet<String>,
        source_label: &str,
        password: Option<&str>,
        consumer: &mut ArchiveEntriesConsumer<'_>,
    ) -> Result<()> {
        for entry_path in entry_paths {
            reader.seek(SeekFrom::Start(0))?;
            let bytes = self.read_entry_bytes_from_reader(
                reader,
                reader_len,
                entry_path,
                source_label,
                password,
            )?;
            consumer(entry_path, &mut Cursor::new(bytes))?;
        }
        Ok(())
    }
}

/// 从本地压缩包及其嵌套容器链路读取指定条目的完整字节，并按容器链路应用密码。
pub(crate) fn read_archive_entry_bytes_with_passwords(
    archive_path: &Path,
    root_format: ArchiveFormat,
    container_entries: &[String],
    entry_path: &str,
    passwords: &ArchivePasswordStore,
) -> Result<Vec<u8>> {
    if container_entries.is_empty() {
        let key = ArchivePasswordKey::root(archive_path.to_path_buf());
        return archive_registry().read_entry_bytes(
            root_format,
            archive_path,
            entry_path,
            passwords.get(&key),
            key,
            archive_path.display().to_string(),
        );
    }

    let first_container = &container_entries[0];
    let mut current_format = root_format;
    let mut current_container_entries: Vec<String> = Vec::new();
    let key = ArchivePasswordKey::root(archive_path.to_path_buf());
    let mut bytes = archive_registry().read_entry_bytes(
        current_format,
        archive_path,
        first_container,
        passwords.get(&key),
        key,
        archive_path.display().to_string(),
    )?;
    let mut current_label = format!("{}!/{first_container}", archive_path.display());
    current_format = detect_container_format(first_container)?;
    current_container_entries.push(first_container.clone());

    for container_entry in &container_entries[1..] {
        let key = ArchivePasswordKey::new(archive_path.to_path_buf(), &current_container_entries);
        bytes = read_archive_entry_bytes_from_reader(
            Cursor::new(bytes),
            current_format,
            container_entry,
            &current_label,
            passwords.get(&key),
            key,
        )?;
        current_label.push_str("!/");
        current_label.push_str(container_entry);
        current_format = detect_container_format(container_entry)?;
        current_container_entries.push(container_entry.clone());
    }

    let key = ArchivePasswordKey::new(archive_path.to_path_buf(), &current_container_entries);
    read_archive_entry_bytes_from_reader(
        Cursor::new(bytes),
        current_format,
        entry_path,
        &current_label,
        passwords.get(&key),
        key,
    )
}

/// 从本地压缩包及其嵌套容器链路流式读取目标日志条目，并按容器链路应用密码。
pub(crate) fn stream_archive_entry_with_passwords(
    archive_path: &Path,
    root_format: ArchiveFormat,
    container_entries: &[String],
    entry_path: &str,
    passwords: &ArchivePasswordStore,
    consumer: &mut ArchiveEntryConsumer<'_>,
) -> Result<()> {
    if container_entries.is_empty() {
        let key = ArchivePasswordKey::root(archive_path.to_path_buf());
        return archive_registry().stream_entry(
            root_format,
            archive_path,
            entry_path,
            passwords.get(&key),
            key,
            archive_path.display().to_string(),
            consumer,
        );
    }

    let first_container = &container_entries[0];
    let mut current_format = root_format;
    let mut current_container_entries: Vec<String> = Vec::new();
    let key = ArchivePasswordKey::root(archive_path.to_path_buf());
    let mut bytes = archive_registry().read_entry_bytes(
        current_format,
        archive_path,
        first_container,
        passwords.get(&key),
        key,
        archive_path.display().to_string(),
    )?;
    let mut current_label = format!("{}!/{first_container}", archive_path.display());
    current_format = detect_container_format(first_container)?;
    current_container_entries.push(first_container.clone());

    for container_entry in &container_entries[1..] {
        let key = ArchivePasswordKey::new(archive_path.to_path_buf(), &current_container_entries);
        bytes = read_archive_entry_bytes_from_reader(
            Cursor::new(bytes),
            current_format,
            container_entry,
            &current_label,
            passwords.get(&key),
            key,
        )?;
        current_label.push_str("!/");
        current_label.push_str(container_entry);
        current_format = detect_container_format(container_entry)?;
        current_container_entries.push(container_entry.clone());
    }

    let key = ArchivePasswordKey::new(archive_path.to_path_buf(), &current_container_entries);
    stream_archive_entry_from_reader(
        Cursor::new(bytes),
        current_format,
        entry_path,
        &current_label,
        passwords.get(&key),
        key,
        consumer,
    )
}

/// 从任意压缩包数据源读取指定条目字节。
fn read_archive_entry_bytes_from_reader<R>(
    mut reader: R,
    format: ArchiveFormat,
    entry_path: &str,
    source_label: &str,
    password: Option<&str>,
    password_key: ArchivePasswordKey,
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
        password,
        password_key,
    )
}

/// 从任意压缩包数据源流式读取指定条目字节。
fn stream_archive_entry_from_reader<R>(
    mut reader: R,
    format: ArchiveFormat,
    entry_path: &str,
    source_label: &str,
    password: Option<&str>,
    password_key: ArchivePasswordKey,
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
        password,
        password_key,
        consumer,
    )
}

/// 根据容器条目名称推导下一层压缩格式。
fn detect_container_format(entry_path: &str) -> Result<ArchiveFormat> {
    archive_registry()
        .detect_name(entry_path)
        .with_context(|| format!("无法识别嵌套压缩包格式：{entry_path}"))
}
