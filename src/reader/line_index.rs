//! 文件职责：建立日志行号到原始字节范围的索引。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：按块扫描大日志文件，记录每行起始偏移和正文长度，供分页读取按需 seek。

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};

/// 行索引扫描块大小；8MiB 能降低系统调用次数，又不会带来明显内存峰值。
const LINE_INDEX_BLOCK_BYTES: usize = 8 * 1024 * 1024;

/// 单行在文件中的字节范围，不包含 LF、CRLF 或 CR 行尾。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LineIndexEntry {
    /// 行正文起始字节偏移。
    pub offset: u64,
    /// 行正文字节长度，不包含换行符。
    pub byte_len: u32,
}

/// 完整日志行索引；entries 使用 `Arc`，便于 reader 句柄在 UI 与后台间共享。
#[derive(Clone, Debug, Default)]
pub(crate) struct LineIndex {
    /// 每一行对应的字节范围。
    entries: Arc<Vec<LineIndexEntry>>,
    /// 最长行的索引，用于横向滚动范围估算。
    longest_line_index: usize,
}

impl LineIndex {
    /// 构造新的行索引。
    ///
    /// 参数说明：
    /// - `entries`：按文件顺序排列的行范围。
    /// - `longest_line_index`：最长行下标。
    ///
    /// 返回值：可共享的行索引对象。
    pub(crate) fn new(entries: Vec<LineIndexEntry>, longest_line_index: usize) -> Self {
        Self {
            entries: Arc::new(entries),
            longest_line_index,
        }
    }

    /// 返回日志行数。
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// 返回索引是否为空。
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 返回最长行索引。
    #[cfg(test)]
    pub(crate) fn longest_line_index(&self) -> usize {
        self.longest_line_index
    }

    /// 返回最长行的原始字节长度，用于 UI 横向滚动范围估算，避免渲染时读取正文。
    pub(crate) fn longest_line_byte_len(&self) -> u32 {
        self.get(self.longest_line_index)
            .map(|entry| entry.byte_len)
            .unwrap_or_default()
    }

    /// 返回指定行的字节范围。
    pub(crate) fn get(&self, line_number: usize) -> Option<LineIndexEntry> {
        self.entries.get(line_number).copied()
    }
}

/// 为本地可 seek 文件建立行索引。
///
/// 参数说明：
/// - `path`：需要分页读取的本地日志或已物化压缩日志路径。
///
/// 返回值：完整行索引；空文件返回 0 行。
#[cfg(test)]
pub(crate) fn build_line_index(path: &Path) -> Result<LineIndex> {
    build_line_index_with_encoding(path, "UTF-8")
}

/// 为本地可 seek 文件按指定编码建立行索引。
///
/// 参数说明：
/// - `path`：需要分页读取的本地日志或已物化压缩日志路径。
/// - `encoding_label`：检测得到的编码名称，用于选择换行扫描策略。
///
/// 返回值：完整行索引；空文件返回 0 行。
pub(crate) fn build_line_index_with_encoding(
    path: &Path,
    encoding_label: &str,
) -> Result<LineIndex> {
    if encoding_label.eq_ignore_ascii_case("UTF-16LE") {
        return build_utf16_line_index(path, Utf16Endian::Little);
    }
    if encoding_label.eq_ignore_ascii_case("UTF-16BE") {
        return build_utf16_line_index(path, Utf16Endian::Big);
    }

    build_byte_line_index(path)
}

/// 为单字节换行编码建立行索引。
fn build_byte_line_index(path: &Path) -> Result<LineIndex> {
    let file = File::open(path).with_context(|| format!("无法打开日志文件：{}", path.display()))?;
    let total_bytes = file
        .metadata()
        .with_context(|| format!("无法读取日志文件元信息：{}", path.display()))?
        .len();
    if total_bytes == 0 {
        return Ok(LineIndex::new(Vec::new(), 0));
    }

    let mut reader = BufReader::with_capacity(LINE_INDEX_BLOCK_BYTES, file);
    let mut buffer = vec![0_u8; LINE_INDEX_BLOCK_BYTES];
    let mut entries = Vec::new();
    let mut absolute_offset = 0_u64;
    let mut line_start = 0_u64;
    let mut line_len = 0_u64;
    let mut longest_line_index = 0_usize;
    let mut longest_line_len = 0_u64;
    let mut skip_lf_after_cr = false;

    loop {
        let read = reader
            .read(&mut buffer)
            .with_context(|| format!("无法扫描日志行索引：{}", path.display()))?;
        if read == 0 {
            break;
        }

        for byte in &buffer[..read] {
            if skip_lf_after_cr {
                skip_lf_after_cr = false;
                if *byte == b'\n' {
                    absolute_offset += 1;
                    line_start = absolute_offset;
                    continue;
                }
            }

            match *byte {
                b'\n' => {
                    push_line_entry(
                        &mut entries,
                        line_start,
                        line_len,
                        &mut longest_line_index,
                        &mut longest_line_len,
                    )?;
                    absolute_offset += 1;
                    line_start = absolute_offset;
                    line_len = 0;
                }
                b'\r' => {
                    push_line_entry(
                        &mut entries,
                        line_start,
                        line_len,
                        &mut longest_line_index,
                        &mut longest_line_len,
                    )?;
                    absolute_offset += 1;
                    line_start = absolute_offset;
                    line_len = 0;
                    skip_lf_after_cr = true;
                }
                _ => {
                    line_len += 1;
                    absolute_offset += 1;
                }
            }
        }
    }

    if line_len > 0 {
        push_line_entry(
            &mut entries,
            line_start,
            line_len,
            &mut longest_line_index,
            &mut longest_line_len,
        )?;
    }

    Ok(LineIndex::new(entries, longest_line_index))
}

/// UTF-16 字节序，用于按双字节编码识别 CR/LF。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Utf16Endian {
    /// UTF-16LE。
    Little,
    /// UTF-16BE。
    Big,
}

/// 为 UTF-16 文件建立行索引，避免按单字节扫描时把换行码元切坏。
fn build_utf16_line_index(path: &Path, endian: Utf16Endian) -> Result<LineIndex> {
    let file = File::open(path).with_context(|| format!("无法打开日志文件：{}", path.display()))?;
    let total_bytes = file
        .metadata()
        .with_context(|| format!("无法读取日志文件元信息：{}", path.display()))?
        .len();
    if total_bytes == 0 {
        return Ok(LineIndex::new(Vec::new(), 0));
    }

    let mut reader = BufReader::with_capacity(LINE_INDEX_BLOCK_BYTES, file);
    let mut buffer = vec![0_u8; LINE_INDEX_BLOCK_BYTES];
    let mut entries = Vec::new();
    let mut absolute_offset = 0_u64;
    let mut line_start = 0_u64;
    let mut line_len = 0_u64;
    let mut longest_line_index = 0_usize;
    let mut longest_line_len = 0_u64;
    let mut skip_lf_after_cr = false;
    let mut pending_byte = None;

    loop {
        let read = reader
            .read(&mut buffer)
            .with_context(|| format!("无法扫描 UTF-16 日志行索引：{}", path.display()))?;
        if read == 0 {
            break;
        }

        let mut index = 0_usize;
        if let Some(first) = pending_byte.take() {
            if read > 0 {
                handle_utf16_unit(
                    [first, buffer[0]],
                    endian,
                    &mut entries,
                    &mut absolute_offset,
                    &mut line_start,
                    &mut line_len,
                    &mut longest_line_index,
                    &mut longest_line_len,
                    &mut skip_lf_after_cr,
                )?;
                index = 1;
            } else {
                pending_byte = Some(first);
            }
        }

        while index + 1 < read {
            handle_utf16_unit(
                [buffer[index], buffer[index + 1]],
                endian,
                &mut entries,
                &mut absolute_offset,
                &mut line_start,
                &mut line_len,
                &mut longest_line_index,
                &mut longest_line_len,
                &mut skip_lf_after_cr,
            )?;
            index += 2;
        }

        if index < read {
            pending_byte = Some(buffer[index]);
        }
    }

    if pending_byte.is_some() {
        line_len += 1;
    }

    if line_len > 0 {
        push_line_entry(
            &mut entries,
            line_start,
            line_len,
            &mut longest_line_index,
            &mut longest_line_len,
        )?;
    }

    Ok(LineIndex::new(entries, longest_line_index))
}

/// 处理一个 UTF-16 码元并在遇到 CR/LF 时追加行索引。
#[allow(clippy::too_many_arguments)]
fn handle_utf16_unit(
    unit_bytes: [u8; 2],
    endian: Utf16Endian,
    entries: &mut Vec<LineIndexEntry>,
    absolute_offset: &mut u64,
    line_start: &mut u64,
    line_len: &mut u64,
    longest_line_index: &mut usize,
    longest_line_len: &mut u64,
    skip_lf_after_cr: &mut bool,
) -> Result<()> {
    let unit = match endian {
        Utf16Endian::Little => u16::from_le_bytes(unit_bytes),
        Utf16Endian::Big => u16::from_be_bytes(unit_bytes),
    };

    if *skip_lf_after_cr {
        *skip_lf_after_cr = false;
        if unit == 0x000a {
            *absolute_offset += 2;
            *line_start = *absolute_offset;
            return Ok(());
        }
    }

    match unit {
        0x000a => {
            push_line_entry(
                entries,
                *line_start,
                *line_len,
                longest_line_index,
                longest_line_len,
            )?;
            *absolute_offset += 2;
            *line_start = *absolute_offset;
            *line_len = 0;
        }
        0x000d => {
            push_line_entry(
                entries,
                *line_start,
                *line_len,
                longest_line_index,
                longest_line_len,
            )?;
            *absolute_offset += 2;
            *line_start = *absolute_offset;
            *line_len = 0;
            *skip_lf_after_cr = true;
        }
        _ => {
            *line_len += 2;
            *absolute_offset += 2;
        }
    }

    Ok(())
}

/// 将当前行追加到索引，并维护最长行位置。
fn push_line_entry(
    entries: &mut Vec<LineIndexEntry>,
    offset: u64,
    byte_len: u64,
    longest_line_index: &mut usize,
    longest_line_len: &mut u64,
) -> Result<()> {
    let byte_len_u32 = u32::try_from(byte_len)
        .map_err(|_| anyhow::anyhow!("单行日志超过 4GB，当前分页索引暂不支持如此长的单行"))?;

    if byte_len > *longest_line_len {
        *longest_line_len = byte_len;
        *longest_line_index = entries.len();
    }

    entries.push(LineIndexEntry {
        offset,
        byte_len: byte_len_u32,
    });
    Ok(())
}

/// 确保行读取范围没有出现整数溢出。
pub(crate) fn checked_line_span(start: LineIndexEntry, end: LineIndexEntry) -> Result<(u64, u64)> {
    let span_end = end
        .offset
        .checked_add(u64::from(end.byte_len))
        .ok_or_else(|| anyhow::anyhow!("日志行字节范围溢出"))?;
    let span_len = span_end
        .checked_sub(start.offset)
        .ok_or_else(|| anyhow::anyhow!("日志行字节范围无效"))?;
    if span_len > usize::MAX as u64 {
        bail!("可见日志范围过大，无法一次读取");
    }

    Ok((start.offset, span_len))
}

#[cfg(test)]
mod tests {
    use super::{build_line_index, build_line_index_with_encoding, checked_line_span};
    use std::fs;
    use std::path::PathBuf;

    /// 构造隔离测试文件路径，避免依赖真实项目文件。
    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("argus-line-index-{}-{name}", std::process::id()))
    }

    /// 验证 LF、CRLF、CR 和末尾无换行都能得到正确字节范围。
    #[test]
    fn indexes_common_newline_styles() {
        let path = temp_path("mixed.log");
        fs::write(&path, b"a\nbb\r\nccc\rdddd").expect("应能写入测试日志");

        let index = build_line_index(&path).expect("应能建立行索引");

        assert_eq!(index.len(), 4);
        assert_eq!(index.get(0).unwrap().byte_len, 1);
        assert_eq!(index.get(1).unwrap().byte_len, 2);
        assert_eq!(index.get(2).unwrap().byte_len, 3);
        assert_eq!(index.get(3).unwrap().byte_len, 4);
        assert_eq!(index.longest_line_index(), 3);

        let _ = fs::remove_file(path);
    }

    /// 验证空文件不会产生虚假空行。
    #[test]
    fn empty_file_has_no_lines() {
        let path = temp_path("empty.log");
        fs::write(&path, []).expect("应能写入空文件");

        let index = build_line_index(&path).expect("应能建立空文件索引");

        assert!(index.is_empty());

        let _ = fs::remove_file(path);
    }

    /// 验证连续可见行范围可被合并为一个安全字节 span。
    #[test]
    fn checked_span_covers_multiple_lines() {
        let path = temp_path("span.log");
        fs::write(&path, b"alpha\nbeta\ngamma").expect("应能写入测试日志");
        let index = build_line_index(&path).expect("应能建立行索引");

        let (offset, len) =
            checked_line_span(index.get(0).unwrap(), index.get(2).unwrap()).unwrap();

        assert_eq!(offset, 0);
        assert_eq!(len, 16);

        let _ = fs::remove_file(path);
    }

    /// 验证 UTF-16LE 日志按双字节换行建立行索引，不会把单字节 0x0A/0x0D 错当边界。
    #[test]
    fn indexes_utf16le_newline_units() {
        let path = temp_path("utf16le.log");
        let mut bytes = vec![0xff, 0xfe];
        for unit in "a\n你\r\nb".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        fs::write(&path, bytes).expect("应能写入 UTF-16LE 日志");

        let index = build_line_index_with_encoding(&path, "UTF-16LE").expect("应能建立行索引");

        assert_eq!(index.len(), 3);
        assert_eq!(index.get(0).unwrap().offset, 0);
        assert_eq!(index.get(0).unwrap().byte_len, 4);
        assert_eq!(index.get(1).unwrap().offset, 6);
        assert_eq!(index.get(1).unwrap().byte_len, 2);
        assert_eq!(index.get(2).unwrap().offset, 12);
        assert_eq!(index.get(2).unwrap().byte_len, 2);

        let _ = fs::remove_file(path);
    }
}
