//! 文件职责：实现 RAR 压缩包目录结构枚举适配器。
//! 创建日期：2026-06-10
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：解析 RAR 条目结构，并通过纯 Rust 解码库读取压缩包内条目字节。

use std::cell::{Cell, RefCell};
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::rc::Rc;

use anyhow::{Context as _, Result, anyhow, bail};
use memmap2::{Mmap, MmapOptions};

use crate::loader::archive::adapter::{
    ArchiveAdapter, ArchiveCapabilities, ArchiveEntryInfo, ArchiveReadSeek,
};
use crate::loader::archive::detector::ArchiveFormat;
use crate::loader::archive::password::{ArchivePasswordError, ArchivePasswordErrorKind};
use crate::utils::path::normalize_archive_entry_path;

/// RAR4 文件签名。
const RAR4_SIGNATURE: &[u8] = b"Rar!\x1A\x07\x00";
/// RAR5 文件签名。
const RAR5_SIGNATURE: &[u8] = b"Rar!\x1A\x07\x01\x00";
/// RAR4 文件块类型。
const RAR4_FILE_BLOCK: u8 = 0x74;
/// RAR4 存储模式；该模式下条目数据无需 RAR 解码即可直接读取。
const RAR4_STORE_METHOD: u8 = 0x30;
/// RAR4 结束块类型。
const RAR4_END_BLOCK: u8 = 0x7B;
/// RAR4 长块标记；非文件块携带附加数据时需要使用该标记跳过数据区。
const RAR4_LONG_BLOCK: u16 = 0x8000;
/// RAR4 大文件标记；出现时文件头包含高 32 位大小。
const RAR4_LARGE_FILE: u16 = 0x0100;
/// RAR4 Unicode 文件名标记；文件名字段前半段仍保留 ANSI 名称，可作为 UI 兜底展示。
const RAR4_UNICODE_NAME: u16 = 0x0200;
/// RAR4 目录标记在字典位上的取值。
const RAR4_DIRECTORY_FLAG: u16 = 0x00E0;
/// RAR4 Windows 目录属性位。
const RAR4_DIRECTORY_ATTR: u32 = 0x10;
/// RAR5 块头包含额外区域大小字段。
const RAR5_HEADER_HAS_EXTRA: u64 = 0x0001;
/// RAR5 块头包含数据区域大小字段。
const RAR5_HEADER_HAS_DATA: u64 = 0x0002;
/// RAR5 文件块类型。
const RAR5_FILE_BLOCK: u64 = 2;
/// RAR5 结束块类型。
const RAR5_END_BLOCK: u64 = 5;
/// RAR5 文件条目为目录。
const RAR5_FILE_IS_DIRECTORY: u64 = 0x0001;
/// RAR5 文件头包含 Unix 时间字段。
const RAR5_FILE_HAS_MTIME: u64 = 0x0002;
/// RAR5 文件头包含 CRC32 字段。
const RAR5_FILE_HAS_CRC32: u64 = 0x0004;
/// RAR5 Windows 目录属性位。
const RAR5_WINDOWS_DIRECTORY_ATTR: u64 = 0x10;
/// RAR5 Unix 目录类型位。
const RAR5_UNIX_DIRECTORY_ATTR: u64 = 0o040000;

/// RAR 适配器，当前仅读取目录结构，不解压条目正文。
#[derive(Debug, Default)]
pub(crate) struct RarArchiveAdapter;

impl ArchiveAdapter for RarArchiveAdapter {
    /// 声明 RAR 格式的识别规则和可用能力。
    fn capabilities(&self) -> ArchiveCapabilities {
        ArchiveCapabilities {
            format: ArchiveFormat::Rar,
            label: "RAR",
            extensions: &[".rar"],
            supports_header_detection: true,
            supports_listing: true,
            supports_entry_reading: true,
            supports_nested_archives: true,
        }
    }

    /// RAR4 与 RAR5 共享 `Rar!\x1A\x07` 前缀。
    fn matches_header(&self, sample: &[u8]) -> bool {
        sample.starts_with(b"Rar!\x1A\x07")
    }

    /// 枚举 RAR 条目并转换为统一条目模型。
    ///
    /// 参数说明：
    /// - `path`：本地 RAR 压缩包路径。
    ///
    /// 返回值：压缩包内条目列表；不执行正文读取或落盘解压。
    fn list_entries(&self, path: &Path, password: Option<&str>) -> Result<Vec<ArchiveEntryInfo>> {
        let bytes = map_rar_file(path)?;
        list_rar_entries_from_bytes(&bytes, &path.display().to_string(), password)
    }

    /// 从内存 RAR 数据源枚举条目。
    fn list_entries_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        _reader_len: u64,
        source_label: &str,
        password: Option<&str>,
    ) -> Result<Vec<ArchiveEntryInfo>> {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .with_context(|| format!("无法读取 RAR 内存压缩包：{source_label}"))?;
        list_rar_entries_from_bytes(&bytes, source_label, password)
    }

    /// 从本地 RAR 读取指定条目字节。
    fn read_entry_bytes(
        &self,
        path: &Path,
        entry_path: &str,
        password: Option<&str>,
    ) -> Result<Vec<u8>> {
        read_rar_entry_bytes(path, entry_path, password)
    }

    /// 从内存 RAR 读取指定条目字节。
    fn read_entry_bytes_from_reader(
        &self,
        reader: &mut dyn ArchiveReadSeek,
        _reader_len: u64,
        entry_path: &str,
        source_label: &str,
        password: Option<&str>,
    ) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .with_context(|| format!("无法读取 RAR 内存压缩包：{source_label}"))?;
        read_rar_entry_bytes_from_bytes(&bytes, entry_path, source_label, password)
    }
}

/// 从本地 RAR 压缩包读取指定条目的完整字节。
///
/// 参数说明：
/// - `path`：本地 RAR 文件路径。
/// - `entry_path`：目标条目路径。
///
/// 返回值：目标条目解包后的完整字节；读取过程不依赖外部命令，默认兼容 Windows。
pub(crate) fn read_rar_entry_bytes(
    path: &Path,
    entry_path: &str,
    password: Option<&str>,
) -> Result<Vec<u8>> {
    let bytes = map_rar_file(path)?;
    read_rar_entry_bytes_from_bytes(&bytes, entry_path, &path.display().to_string(), password)
}

/// 将本地 RAR 文件映射为只读字节视图，避免加载目录树时把大压缩包整包复制到堆内存。
fn map_rar_file(path: &Path) -> Result<Mmap> {
    let file =
        File::open(path).with_context(|| format!("无法打开 RAR 压缩包：{}", path.display()))?;
    let file_size = file
        .metadata()
        .with_context(|| format!("无法读取 RAR 文件大小：{}", path.display()))?
        .len();
    if file_size == 0 {
        bail!("RAR 压缩包为空：{}", path.display());
    }

    // SAFETY: 这里只创建只读映射，不写入文件；映射对象持有操作系统映射句柄，
    // 返回后不依赖 `file` 变量生命周期。若文件被外部进程并发截断，底层 mmap
    // 仍可能由操作系统报错，这是所有 mmap 读取共有的边界。
    unsafe { MmapOptions::new().map(&file) }
        .with_context(|| format!("无法映射 RAR 压缩包：{}", path.display()))
}

/// 从 RAR 原始字节枚举条目，便于本地文件读取与单元测试复用。
///
/// 参数说明：
/// - `bytes`：RAR 压缩包完整字节。
/// - `source_label`：错误提示中的来源名称。
///
/// 返回值：RAR 内部目录和文件条目。
pub(crate) fn list_rar_entries_from_bytes(
    bytes: &[u8],
    source_label: &str,
    password: Option<&str>,
) -> Result<Vec<ArchiveEntryInfo>> {
    match list_rar_entries_with_rars(bytes, source_label, password) {
        Ok(entries) => Ok(entries),
        Err(rars_error) if is_archive_password_error(&rars_error) => Err(rars_error),
        Err(rars_error) => list_rar_entries_direct(bytes, source_label).map_err(|direct_error| {
            anyhow!("纯 Rust RAR 库枚举失败：{rars_error}；内置头解析兜底也失败：{direct_error}")
        }),
    }
}

/// 从 RAR 原始字节读取指定条目内容。
///
/// 参数说明：
/// - `bytes`：RAR 压缩包完整字节。
/// - `entry_path`：目标条目路径。
/// - `source_label`：错误提示中的来源名称。
///
/// 返回值：条目完整字节；读取过程使用纯 Rust 解码库，压缩算法条目也可被解包。
pub(crate) fn read_rar_entry_bytes_from_bytes(
    bytes: &[u8],
    entry_path: &str,
    source_label: &str,
    password: Option<&str>,
) -> Result<Vec<u8>> {
    match read_rar_entry_bytes_with_rars(bytes, entry_path, source_label, password) {
        Ok(bytes) => Ok(bytes),
        Err(rars_error) if is_archive_password_error(&rars_error) => Err(rars_error),
        Err(rars_error) => {
            read_rar_entry_bytes_direct(bytes, entry_path, source_label).map_err(|direct_error| {
                anyhow!(
                    "纯 Rust RAR 库读取失败：{rars_error}；内置存储模式兜底也失败：{direct_error}"
                )
            })
        }
    }
}

/// 使用 rars 从 RAR 原始字节枚举条目；该库不依赖系统命令，适合跨平台默认路径。
fn list_rar_entries_with_rars(
    bytes: &[u8],
    source_label: &str,
    password: Option<&str>,
) -> Result<Vec<ArchiveEntryInfo>> {
    let archive = read_rars_archive(bytes, source_label, password)?;
    let mut entries = Vec::new();
    let mut has_encrypted_member = false;

    for member in archive.members() {
        has_encrypted_member |= member.meta.is_encrypted;
        let entry_path = normalize_archive_entry_path(&member.meta.name_lossy());
        if entry_path.is_empty() {
            continue;
        }

        entries.push(build_entry(
            entry_path,
            member.meta.is_directory,
            Some(member.meta.unpacked_size),
        ));
    }

    if has_encrypted_member && password.is_none() {
        return Err(ArchivePasswordError::required(source_label).into());
    }

    Ok(entries)
}

/// 使用 rars 从 RAR 原始字节读取目标条目内容。
fn read_rar_entry_bytes_with_rars(
    bytes: &[u8],
    entry_path: &str,
    source_label: &str,
    password: Option<&str>,
) -> Result<Vec<u8>> {
    let archive = read_rars_archive(bytes, source_label, password)?;
    let normalized_entry_path = normalize_archive_entry_path(entry_path);
    let password_bytes = password.map(str::as_bytes);

    match &archive {
        rars::Archive::Rar13(rar13_archive) => {
            read_rar13_target_entry_bytes(rar13_archive, &normalized_entry_path, password_bytes)
        }
        rars::Archive::Rar15To40(rar15_archive) if rar15_archive.main.is_solid() => {
            read_rar_entry_bytes_by_streaming_archive(
                &archive,
                &normalized_entry_path,
                password_bytes,
            )
        }
        rars::Archive::Rar15To40(rar15_archive) => read_rar15_to_40_target_entry_bytes(
            rar15_archive,
            &normalized_entry_path,
            password_bytes,
        ),
        rars::Archive::Rar50Plus(rar50_archive) if rar50_archive.main.is_solid() => {
            read_rar_entry_bytes_by_streaming_archive(
                &archive,
                &normalized_entry_path,
                password_bytes,
            )
        }
        rars::Archive::Rar50Plus(rar50_archive) => {
            read_rar50_target_entry_bytes(rar50_archive, &normalized_entry_path, password_bytes)
        }
        _ => bail!("暂不支持该 RAR 家族读取条目：{normalized_entry_path}"),
    }
}

/// 解析 RAR 原始字节为 rars 归档对象，并统一错误上下文。
fn read_rars_archive(
    bytes: &[u8],
    source_label: &str,
    password: Option<&str>,
) -> Result<rars::Archive> {
    rars::ArchiveReader::read_with_options(
        bytes,
        rars::ArchiveReadOptions::with_optional_password(password.map(str::as_bytes)),
    )
    .map_err(|error| map_rar_error(error, source_label))
    .with_context(|| format!("无法解析 RAR 压缩包 {source_label}"))
}

/// 顺序流式读取目标条目；solid RAR 需要保留前序文件解码上下文，因此不能直接跳到目标成员。
fn read_rar_entry_bytes_by_streaming_archive(
    archive: &rars::Archive,
    normalized_entry_path: &str,
    password: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let captured_bytes = Rc::new(RefCell::new(Vec::new()));
    let did_open_target = Rc::new(Cell::new(false));
    let captured_bytes_for_writer = Rc::clone(&captured_bytes);
    let did_open_target_for_writer = Rc::clone(&did_open_target);
    let normalized_entry_path_for_writer = normalized_entry_path.to_string();

    archive
        .extract_to(password, |meta| {
            let current_path = normalize_archive_entry_path(&meta.name_lossy());
            if current_path == normalized_entry_path_for_writer && !meta.is_directory {
                did_open_target_for_writer.set(true);
                Ok(Box::new(RarCapturedWriter {
                    bytes: Rc::clone(&captured_bytes_for_writer),
                }) as Box<dyn Write>)
            } else {
                Ok(Box::new(io::sink()) as Box<dyn Write>)
            }
        })
        .map_err(|error| map_rar_error(error, normalized_entry_path))
        .with_context(|| format!("无法解码 RAR 条目 {normalized_entry_path}"))?;

    if !did_open_target.get() {
        bail!("RAR 解码过程未输出目标条目：{normalized_entry_path}");
    }

    Ok(captured_bytes.borrow().clone())
}

/// rars 顺序解码回调使用的内存写入器，只捕获当前目标条目的解包字节。
struct RarCapturedWriter {
    /// 解码输出共享缓冲区；rars 的 writer 由回调返回，因此需要所有权共享。
    bytes: Rc<RefCell<Vec<u8>>>,
}

impl Write for RarCapturedWriter {
    /// 将 rars 输出的解包分片追加到目标缓冲区。
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes.borrow_mut().extend_from_slice(buffer);
        Ok(buffer.len())
    }

    /// 内存缓冲区无需刷新，但实现该方法以满足 `Write` trait。
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 从 RAR1.3/1.4 归档中只解码目标条目。
fn read_rar13_target_entry_bytes(
    archive: &rars::rar13::Archive,
    normalized_entry_path: &str,
    password: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let entry = archive
        .entries
        .iter()
        .find(|entry| normalize_archive_entry_path(&entry.name_lossy()) == normalized_entry_path)
        .with_context(|| format!("未找到 RAR 条目：{normalized_entry_path}"))?;
    if entry.is_directory() {
        bail!("RAR 条目是目录，无法读取内容：{normalized_entry_path}");
    }

    let mut bytes = Vec::new();
    entry
        .write_to(archive, password, &mut bytes)
        .map_err(|error| map_rar_error(error, normalized_entry_path))
        .with_context(|| format!("无法解码 RAR 条目 {normalized_entry_path}"))?;
    Ok(bytes)
}

/// 从 RAR1.5 到 RAR4.x 归档中只解码目标条目。
fn read_rar15_to_40_target_entry_bytes(
    archive: &rars::rar15_40::Archive,
    normalized_entry_path: &str,
    password: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let entry = archive
        .files()
        .find(|entry| normalize_archive_entry_path(&entry.name_lossy()) == normalized_entry_path)
        .with_context(|| format!("未找到 RAR 条目：{normalized_entry_path}"))?;
    if entry.is_directory() {
        bail!("RAR 条目是目录，无法读取内容：{normalized_entry_path}");
    }

    let mut bytes = Vec::new();
    entry
        .write_to(archive, password, &mut bytes)
        .map_err(|error| map_rar_error(error, normalized_entry_path))
        .with_context(|| format!("无法解码 RAR 条目 {normalized_entry_path}"))?;
    Ok(bytes)
}

/// 从 RAR5+ 归档中只解码目标条目。
fn read_rar50_target_entry_bytes(
    archive: &rars::rar50::Archive,
    normalized_entry_path: &str,
    password: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let entry = archive
        .files()
        .find(|entry| normalize_archive_entry_path(&entry.name_lossy()) == normalized_entry_path)
        .with_context(|| format!("未找到 RAR 条目：{normalized_entry_path}"))?;
    if entry.is_directory() {
        bail!("RAR 条目是目录，无法读取内容：{normalized_entry_path}");
    }

    let mut bytes = Vec::new();
    entry
        .write_to(archive, password, &mut bytes)
        .map_err(|error| map_rar_error(error, normalized_entry_path))
        .with_context(|| format!("无法解码 RAR 条目 {normalized_entry_path}"))?;
    Ok(bytes)
}

/// 判断 anyhow 错误链中是否已包含统一密码错误。
fn is_archive_password_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<ArchivePasswordError>().is_some())
}

/// 将 rars 的密码相关错误转换为 Argus 统一错误。
fn map_rar_error(error: rars::Error, source_label: &str) -> anyhow::Error {
    match rar_password_error_kind(&error) {
        Some(ArchivePasswordErrorKind::Required) => {
            ArchivePasswordError::required(source_label).into()
        }
        Some(ArchivePasswordErrorKind::Invalid) => {
            ArchivePasswordError::invalid(source_label).into()
        }
        Some(ArchivePasswordErrorKind::Unsupported) => {
            ArchivePasswordError::unsupported(source_label, error.to_string()).into()
        }
        None => error.into(),
    }
}

/// 递归识别 rars 错误中的密码失败类型。
fn rar_password_error_kind(error: &rars::Error) -> Option<ArchivePasswordErrorKind> {
    match error {
        rars::Error::NeedPassword => Some(ArchivePasswordErrorKind::Required),
        rars::Error::WrongPasswordOrCorruptData => Some(ArchivePasswordErrorKind::Invalid),
        rars::Error::UnsupportedEncryption { .. } => Some(ArchivePasswordErrorKind::Unsupported),
        rars::Error::AtArchiveOffset { source, .. } | rars::Error::AtEntry { source, .. } => {
            rar_password_error_kind(source)
        }
        _ => None,
    }
}

/// 使用内置轻量头解析枚举 RAR 条目；仅作为第三方库无法解析时的兜底。
fn list_rar_entries_direct(bytes: &[u8], source_label: &str) -> Result<Vec<ArchiveEntryInfo>> {
    if bytes.starts_with(RAR5_SIGNATURE) {
        return parse_rar5_entries(bytes, source_label);
    }

    if bytes.starts_with(RAR4_SIGNATURE) {
        return parse_rar4_entries(bytes, source_label);
    }

    bail!("无法识别 RAR 压缩包签名：{source_label}")
}

/// 直接从 RAR 原始字节读取指定条目内容，仅支持无需解码的存储模式兜底。
fn read_rar_entry_bytes_direct(
    bytes: &[u8],
    entry_path: &str,
    source_label: &str,
) -> Result<Vec<u8>> {
    if bytes.starts_with(RAR5_SIGNATURE) {
        return read_rar5_entry_bytes(bytes, entry_path, source_label);
    }

    if bytes.starts_with(RAR4_SIGNATURE) {
        return read_rar4_entry_bytes(bytes, entry_path, source_label);
    }

    bail!("无法识别 RAR 压缩包签名：{source_label}")
}

/// RAR4 公共块头；文件块会在该头之后继续携带文件元数据。
#[derive(Clone, Copy, Debug)]
struct Rar4BlockHeader {
    /// 块类型。
    header_type: u8,
    /// 块标记。
    flags: u16,
    /// 块头大小，不包含文件数据区。
    header_size: usize,
}

/// 解析 RAR4 条目列表。
fn parse_rar4_entries(bytes: &[u8], source_label: &str) -> Result<Vec<ArchiveEntryInfo>> {
    let mut offset = RAR4_SIGNATURE.len();
    let mut entries = Vec::new();

    while offset + 7 <= bytes.len() {
        let header = read_rar4_block_header(bytes, offset, source_label)?;
        let header_end = checked_add(offset, header.header_size, source_label)?;
        if header_end > bytes.len() {
            bail!("RAR4 块头越界：{source_label}");
        }

        let data_size = if header.header_type == RAR4_FILE_BLOCK {
            parse_rar4_file_block(
                bytes,
                offset,
                header,
                header_end,
                &mut entries,
                source_label,
            )?
        } else {
            rar4_generic_data_size(bytes, offset, header, header_end, source_label)?
        };

        if header.header_type == RAR4_END_BLOCK {
            break;
        }

        offset = checked_add(header_end, data_size, source_label)?;
        if offset > bytes.len() {
            bail!("RAR4 数据区越界：{source_label}");
        }
    }

    Ok(entries)
}

/// 从 RAR4 压缩包中读取目标条目字节。
fn read_rar4_entry_bytes(bytes: &[u8], entry_path: &str, source_label: &str) -> Result<Vec<u8>> {
    let normalized_entry_path = normalize_archive_entry_path(entry_path);
    let mut offset = RAR4_SIGNATURE.len();

    while offset + 7 <= bytes.len() {
        let header = read_rar4_block_header(bytes, offset, source_label)?;
        let header_end = checked_add(offset, header.header_size, source_label)?;
        if header_end > bytes.len() {
            bail!("RAR4 块头越界：{source_label}");
        }

        let data_size = if header.header_type == RAR4_FILE_BLOCK {
            let file_info = read_rar4_file_info(bytes, offset, header, header_end, source_label)?;
            let data_end = checked_add(header_end, file_info.packed_size, source_label)?;
            if data_end > bytes.len() {
                bail!("RAR4 文件数据越界：{source_label}");
            }

            if file_info.entry_path == normalized_entry_path {
                if file_info.is_dir {
                    bail!("RAR4 条目是目录，无法读取内容：{normalized_entry_path}");
                }
                if file_info.method != RAR4_STORE_METHOD
                    || file_info.packed_size as u64 != file_info.unpacked_size
                {
                    bail!("RAR4 条目使用压缩算法，暂无法直接读取：{normalized_entry_path}");
                }
                return Ok(bytes[header_end..data_end].to_vec());
            }

            file_info.packed_size
        } else {
            rar4_generic_data_size(bytes, offset, header, header_end, source_label)?
        };

        if header.header_type == RAR4_END_BLOCK {
            break;
        }

        offset = checked_add(header_end, data_size, source_label)?;
        if offset > bytes.len() {
            bail!("RAR4 数据区越界：{source_label}");
        }
    }

    bail!("未找到 RAR4 条目：{normalized_entry_path}")
}

/// RAR4 文件块中可复用的条目信息。
#[derive(Debug)]
struct Rar4FileInfo {
    /// 规范化条目路径。
    entry_path: String,
    /// 是否为目录。
    is_dir: bool,
    /// 压缩数据大小。
    packed_size: usize,
    /// 解包后大小。
    unpacked_size: u64,
    /// RAR4 压缩方法。
    method: u8,
}

/// 读取 RAR4 公共块头。
fn read_rar4_block_header(
    bytes: &[u8],
    offset: usize,
    source_label: &str,
) -> Result<Rar4BlockHeader> {
    let header_type = *bytes
        .get(offset + 2)
        .with_context(|| format!("RAR4 块类型缺失：{source_label}"))?;
    let flags = read_u16_le(bytes, offset + 3, source_label)?;
    let header_size = read_u16_le(bytes, offset + 5, source_label)? as usize;
    if header_size < 7 {
        bail!("RAR4 块头大小异常：{source_label}");
    }

    Ok(Rar4BlockHeader {
        header_type,
        flags,
        header_size,
    })
}

/// 解析 RAR4 文件块并返回需要跳过的压缩数据大小。
fn parse_rar4_file_block(
    bytes: &[u8],
    block_offset: usize,
    header: Rar4BlockHeader,
    header_end: usize,
    entries: &mut Vec<ArchiveEntryInfo>,
    source_label: &str,
) -> Result<usize> {
    let file_info = read_rar4_file_info(bytes, block_offset, header, header_end, source_label)?;

    if !file_info.entry_path.is_empty() {
        entries.push(build_entry(
            file_info.entry_path,
            file_info.is_dir,
            Some(file_info.unpacked_size),
        ));
    }

    Ok(file_info.packed_size)
}

/// 读取 RAR4 文件块中目录树与数据定位需要的字段。
fn read_rar4_file_info(
    bytes: &[u8],
    block_offset: usize,
    header: Rar4BlockHeader,
    header_end: usize,
    source_label: &str,
) -> Result<Rar4FileInfo> {
    if header.header_size < 32 {
        bail!("RAR4 文件块头过短：{source_label}");
    }

    let packed_size_low = read_u32_le(bytes, block_offset + 7, source_label)? as u64;
    let unpacked_size_low = read_u32_le(bytes, block_offset + 11, source_label)? as u64;
    let method = *bytes
        .get(block_offset + 25)
        .with_context(|| format!("RAR4 压缩方法缺失：{source_label}"))?;
    let name_size = read_u16_le(bytes, block_offset + 26, source_label)? as usize;
    let attributes = read_u32_le(bytes, block_offset + 28, source_label)?;
    let mut packed_size = packed_size_low;
    let mut unpacked_size = unpacked_size_low;
    let mut name_offset = block_offset + 32;

    if header.flags & RAR4_LARGE_FILE != 0 {
        let packed_size_high = read_u32_le(bytes, block_offset + 32, source_label)? as u64;
        let unpacked_size_high = read_u32_le(bytes, block_offset + 36, source_label)? as u64;
        packed_size |= packed_size_high << 32;
        unpacked_size |= unpacked_size_high << 32;
        name_offset += 8;
    }

    let name_end = checked_add(name_offset, name_size, source_label)?;
    if name_end > header_end {
        bail!("RAR4 文件名越界：{source_label}");
    }

    let raw_name = &bytes[name_offset..name_end];
    let original_name = decode_rar4_name(raw_name, header.flags & RAR4_UNICODE_NAME != 0);
    let entry_path = normalize_archive_entry_path(&original_name);
    let is_dir = is_rar4_directory(&original_name, header.flags, attributes);
    let packed_size =
        usize::try_from(packed_size).with_context(|| format!("RAR4 数据区过大：{source_label}"))?;

    Ok(Rar4FileInfo {
        entry_path,
        is_dir,
        packed_size,
        unpacked_size,
        method,
    })
}

/// 计算 RAR4 非文件块需要跳过的数据大小。
fn rar4_generic_data_size(
    bytes: &[u8],
    block_offset: usize,
    header: Rar4BlockHeader,
    header_end: usize,
    source_label: &str,
) -> Result<usize> {
    if header.flags & RAR4_LONG_BLOCK == 0 {
        return Ok(0);
    }

    if block_offset + 11 > header_end {
        bail!("RAR4 长块附加大小缺失：{source_label}");
    }

    Ok(read_u32_le(bytes, block_offset + 7, source_label)? as usize)
}

/// 解码 RAR4 文件名；Unicode 扩展名采用前置原始名称作为无需完整编码器的稳定兜底。
fn decode_rar4_name(raw_name: &[u8], has_unicode_name: bool) -> String {
    if has_unicode_name && let Some(split_index) = raw_name.iter().position(|byte| *byte == 0) {
        return String::from_utf8_lossy(&raw_name[..split_index]).into_owned();
    }

    String::from_utf8_lossy(raw_name).into_owned()
}

/// 判断 RAR4 条目是否为目录。
fn is_rar4_directory(original_name: &str, flags: u16, attributes: u32) -> bool {
    original_name.ends_with('/')
        || original_name.ends_with('\\')
        || attributes & RAR4_DIRECTORY_ATTR != 0
        || flags & RAR4_DIRECTORY_FLAG == RAR4_DIRECTORY_FLAG
}

/// 解析 RAR5 条目列表。
fn parse_rar5_entries(bytes: &[u8], source_label: &str) -> Result<Vec<ArchiveEntryInfo>> {
    let mut offset = RAR5_SIGNATURE.len();
    let mut entries = Vec::new();

    while offset < bytes.len() {
        if offset + 4 > bytes.len() {
            break;
        }

        // RAR5 每个块前 4 字节是头 CRC；目录树只需要结构字段，校验由后续读取流程兜底。
        offset += 4;
        let header_size = read_rar5_vint(bytes, &mut offset, bytes.len(), source_label)?;
        let header_size = usize::try_from(header_size)
            .with_context(|| format!("RAR5 块头过大：{source_label}"))?;
        if header_size == 0 {
            bail!("RAR5 块头为空：{source_label}");
        }

        let header_start = offset;
        let header_end = checked_add(header_start, header_size, source_label)?;
        if header_end > bytes.len() {
            bail!("RAR5 块头越界：{source_label}");
        }

        let mut cursor = header_start;
        let header_type = read_rar5_vint(bytes, &mut cursor, header_end, source_label)?;
        let header_flags = read_rar5_vint(bytes, &mut cursor, header_end, source_label)?;

        if header_flags & RAR5_HEADER_HAS_EXTRA != 0 {
            let _extra_area_size = read_rar5_vint(bytes, &mut cursor, header_end, source_label)?;
        }
        let data_area_size = if header_flags & RAR5_HEADER_HAS_DATA != 0 {
            read_rar5_vint(bytes, &mut cursor, header_end, source_label)?
        } else {
            0
        };

        if header_type == RAR5_FILE_BLOCK {
            parse_rar5_file_block(bytes, &mut cursor, header_end, &mut entries, source_label)?;
        }

        offset = checked_add(
            header_end,
            usize::try_from(data_area_size)
                .with_context(|| format!("RAR5 数据区过大：{source_label}"))?,
            source_label,
        )?;
        if offset > bytes.len() {
            bail!("RAR5 数据区越界：{source_label}");
        }

        if header_type == RAR5_END_BLOCK {
            break;
        }
    }

    Ok(entries)
}

/// 从 RAR5 压缩包中读取目标条目字节。
fn read_rar5_entry_bytes(bytes: &[u8], entry_path: &str, source_label: &str) -> Result<Vec<u8>> {
    let normalized_entry_path = normalize_archive_entry_path(entry_path);
    let mut offset = RAR5_SIGNATURE.len();

    while offset < bytes.len() {
        if offset + 4 > bytes.len() {
            break;
        }

        offset += 4;
        let header_size = read_rar5_vint(bytes, &mut offset, bytes.len(), source_label)?;
        let header_size = usize::try_from(header_size)
            .with_context(|| format!("RAR5 块头过大：{source_label}"))?;
        if header_size == 0 {
            bail!("RAR5 块头为空：{source_label}");
        }

        let header_start = offset;
        let header_end = checked_add(header_start, header_size, source_label)?;
        if header_end > bytes.len() {
            bail!("RAR5 块头越界：{source_label}");
        }

        let mut cursor = header_start;
        let header_type = read_rar5_vint(bytes, &mut cursor, header_end, source_label)?;
        let header_flags = read_rar5_vint(bytes, &mut cursor, header_end, source_label)?;

        if header_flags & RAR5_HEADER_HAS_EXTRA != 0 {
            let _extra_area_size = read_rar5_vint(bytes, &mut cursor, header_end, source_label)?;
        }
        let data_area_size = if header_flags & RAR5_HEADER_HAS_DATA != 0 {
            read_rar5_vint(bytes, &mut cursor, header_end, source_label)?
        } else {
            0
        };
        let data_size = usize::try_from(data_area_size)
            .with_context(|| format!("RAR5 数据区过大：{source_label}"))?;
        let data_end = checked_add(header_end, data_size, source_label)?;
        if data_end > bytes.len() {
            bail!("RAR5 数据区越界：{source_label}");
        }

        if header_type == RAR5_FILE_BLOCK {
            let file_info = read_rar5_file_info(bytes, &mut cursor, header_end, source_label)?;
            if file_info.entry_path == normalized_entry_path {
                if file_info.is_dir {
                    bail!("RAR5 条目是目录，无法读取内容：{normalized_entry_path}");
                }
                if data_area_size != file_info.unpacked_size {
                    bail!("RAR5 条目使用压缩算法，暂无法直接读取：{normalized_entry_path}");
                }
                return Ok(bytes[header_end..data_end].to_vec());
            }
        }

        offset = data_end;

        if header_type == RAR5_END_BLOCK {
            break;
        }
    }

    bail!("未找到 RAR5 条目：{normalized_entry_path}")
}

/// RAR5 文件块中可复用的条目信息。
#[derive(Debug)]
struct Rar5FileInfo {
    /// 规范化条目路径。
    entry_path: String,
    /// 是否为目录。
    is_dir: bool,
    /// 解包后大小。
    unpacked_size: u64,
}

/// 解析 RAR5 文件块中的目录树相关字段。
fn parse_rar5_file_block(
    bytes: &[u8],
    cursor: &mut usize,
    header_end: usize,
    entries: &mut Vec<ArchiveEntryInfo>,
    source_label: &str,
) -> Result<()> {
    let file_info = read_rar5_file_info(bytes, cursor, header_end, source_label)?;
    if file_info.entry_path.is_empty() {
        return Ok(());
    }

    entries.push(build_entry(
        file_info.entry_path,
        file_info.is_dir,
        Some(file_info.unpacked_size),
    ));
    Ok(())
}

/// 读取 RAR5 文件块中目录树与数据定位需要的字段。
fn read_rar5_file_info(
    bytes: &[u8],
    cursor: &mut usize,
    header_end: usize,
    source_label: &str,
) -> Result<Rar5FileInfo> {
    let file_flags = read_rar5_vint(bytes, cursor, header_end, source_label)?;
    let unpacked_size = read_rar5_vint(bytes, cursor, header_end, source_label)?;
    let attributes = read_rar5_vint(bytes, cursor, header_end, source_label)?;

    if file_flags & RAR5_FILE_HAS_MTIME != 0 {
        skip_bytes(cursor, 4, header_end, source_label)?;
    }
    if file_flags & RAR5_FILE_HAS_CRC32 != 0 {
        skip_bytes(cursor, 4, header_end, source_label)?;
    }

    let _compression_info = read_rar5_vint(bytes, cursor, header_end, source_label)?;
    let _host_os = read_rar5_vint(bytes, cursor, header_end, source_label)?;
    let name_size = read_rar5_vint(bytes, cursor, header_end, source_label)?;
    let name_size =
        usize::try_from(name_size).with_context(|| format!("RAR5 文件名过长：{source_label}"))?;
    let name_end = checked_add(*cursor, name_size, source_label)?;
    if name_end > header_end {
        bail!("RAR5 文件名越界：{source_label}");
    }

    let original_name = String::from_utf8_lossy(&bytes[*cursor..name_end]).into_owned();
    *cursor = name_end;
    let entry_path = normalize_archive_entry_path(&original_name);

    let is_dir = original_name.ends_with('/')
        || original_name.ends_with('\\')
        || file_flags & RAR5_FILE_IS_DIRECTORY != 0
        || attributes & RAR5_WINDOWS_DIRECTORY_ATTR != 0
        || attributes & RAR5_UNIX_DIRECTORY_ATTR == RAR5_UNIX_DIRECTORY_ATTR;
    Ok(Rar5FileInfo {
        entry_path,
        is_dir,
        unpacked_size,
    })
}

/// 读取 RAR5 可变长度整数，最多 10 字节以覆盖完整 u64。
fn read_rar5_vint(
    bytes: &[u8],
    cursor: &mut usize,
    limit: usize,
    source_label: &str,
) -> Result<u64> {
    let mut value = 0_u64;
    let mut shift = 0_u32;

    for _ in 0..10 {
        let byte = *bytes
            .get(*cursor)
            .filter(|_| *cursor < limit)
            .with_context(|| format!("RAR5 可变整数越界：{source_label}"))?;
        *cursor += 1;
        value |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
    }

    bail!("RAR5 可变整数过长：{source_label}")
}

/// 跳过固定长度字段，并保证不会越过当前块头边界。
fn skip_bytes(
    cursor: &mut usize,
    byte_count: usize,
    limit: usize,
    source_label: &str,
) -> Result<()> {
    *cursor = checked_add(*cursor, byte_count, source_label)?;
    if *cursor > limit {
        bail!("RAR5 文件块字段越界：{source_label}");
    }
    Ok(())
}

/// 构建统一压缩包条目模型。
fn build_entry(path: String, is_dir: bool, size: Option<u64>) -> ArchiveEntryInfo {
    ArchiveEntryInfo {
        path,
        is_dir,
        size: if is_dir { None } else { size },
    }
}

/// 读取小端 u16。
fn read_u16_le(bytes: &[u8], offset: usize, source_label: &str) -> Result<u16> {
    let value = bytes
        .get(offset..offset + 2)
        .with_context(|| format!("RAR 字段越界：{source_label}"))?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

/// 读取小端 u32。
fn read_u32_le(bytes: &[u8], offset: usize, source_label: &str) -> Result<u32> {
    let value = bytes
        .get(offset..offset + 4)
        .with_context(|| format!("RAR 字段越界：{source_label}"))?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

/// 计算偏移并捕获整数溢出，避免异常压缩包触发 panic。
fn checked_add(left: usize, right: usize, source_label: &str) -> Result<usize> {
    left.checked_add(right)
        .with_context(|| format!("RAR 偏移计算溢出：{source_label}"))
}

#[cfg(test)]
mod tests {
    use super::{RAR4_SIGNATURE, RAR5_SIGNATURE, list_rar_entries_from_bytes};

    /// 写入小端 u16。
    fn push_u16(bytes: &mut Vec<u8>, value: u16) {
        bytes.extend(value.to_le_bytes());
    }

    /// 写入小端 u32。
    fn push_u32(bytes: &mut Vec<u8>, value: u32) {
        bytes.extend(value.to_le_bytes());
    }

    /// 写入 RAR5 可变长度整数。
    fn push_vint(bytes: &mut Vec<u8>, mut value: u64) {
        loop {
            let mut byte = (value & 0x7F) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            bytes.push(byte);
            if value == 0 {
                break;
            }
        }
    }

    /// 构造 RAR4 文件块，测试只依赖目录树元数据，不需要真实压缩数据。
    fn push_rar4_file(bytes: &mut Vec<u8>, name: &str, is_dir: bool, unpacked_size: u32) {
        let header_size = 32 + name.len();
        push_u16(bytes, 0);
        bytes.push(0x74);
        push_u16(bytes, if is_dir { 0x00E0 } else { 0 });
        push_u16(bytes, header_size as u16);
        push_u32(bytes, 0);
        push_u32(bytes, unpacked_size);
        bytes.push(3);
        push_u32(bytes, 0);
        push_u32(bytes, 0);
        bytes.push(20);
        bytes.push(0x30);
        push_u16(bytes, name.len() as u16);
        push_u32(bytes, if is_dir { 0x10 } else { 0x20 });
        bytes.extend(name.as_bytes());
    }

    /// 构造 RAR4 结束块。
    fn push_rar4_end(bytes: &mut Vec<u8>) {
        push_u16(bytes, 0);
        bytes.push(0x7B);
        push_u16(bytes, 0);
        push_u16(bytes, 7);
    }

    /// 构造 RAR5 通用块，CRC 字段使用零值占位即可。
    fn push_rar5_block(bytes: &mut Vec<u8>, body: Vec<u8>, data_size: usize) {
        bytes.extend([0, 0, 0, 0]);
        push_vint(bytes, body.len() as u64);
        bytes.extend(body);
        bytes.extend(std::iter::repeat_n(0, data_size));
    }

    /// 构造 RAR5 文件块体。
    fn rar5_file_body(name: &str, is_dir: bool, unpacked_size: u64) -> Vec<u8> {
        let mut body = Vec::new();
        push_vint(&mut body, 2);
        push_vint(&mut body, 0);
        push_vint(&mut body, if is_dir { 1 } else { 0 });
        push_vint(&mut body, unpacked_size);
        push_vint(&mut body, if is_dir { 0x10 } else { 0x20 });
        push_vint(&mut body, 0);
        push_vint(&mut body, 2);
        push_vint(&mut body, name.len() as u64);
        body.extend(name.as_bytes());
        body
    }

    /// 验证 RAR4 文件头可生成目录和文件条目。
    #[test]
    fn lists_rar4_entries_from_headers() {
        let mut bytes = RAR4_SIGNATURE.to_vec();
        push_u16(&mut bytes, 0);
        bytes.push(0x73);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 7);
        push_rar4_file(&mut bytes, "logs/", true, 0);
        push_rar4_file(&mut bytes, "logs/app.log", false, 12);
        push_rar4_end(&mut bytes);

        let entries =
            list_rar_entries_from_bytes(&bytes, "fixture.rar", None).expect("应能解析 RAR4");

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, "logs");
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].path, "logs/app.log");
        assert_eq!(entries[1].size, Some(12));
    }

    /// 验证 RAR5 文件头可生成目录和文件条目。
    #[test]
    fn lists_rar5_entries_from_headers() {
        let mut bytes = RAR5_SIGNATURE.to_vec();
        let mut main_body = Vec::new();
        push_vint(&mut main_body, 1);
        push_vint(&mut main_body, 0);
        push_vint(&mut main_body, 0);
        push_rar5_block(&mut bytes, main_body, 0);
        push_rar5_block(&mut bytes, rar5_file_body("logs", true, 0), 0);
        push_rar5_block(&mut bytes, rar5_file_body("logs/app.log", false, 42), 0);

        let mut end_body = Vec::new();
        push_vint(&mut end_body, 5);
        push_vint(&mut end_body, 0);
        push_rar5_block(&mut bytes, end_body, 0);

        let entries =
            list_rar_entries_from_bytes(&bytes, "fixture.rar", None).expect("应能解析 RAR5");

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, "logs");
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].path, "logs/app.log");
        assert_eq!(entries[1].size, Some(42));
    }
}
