//! 文件职责：按固定字节预算读取日志行窗口，跳过和超长行也保持常量内存。

use std::path::Path;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader};
use tokio_util::sync::CancellationToken;

use crate::log_io::encoding_detector::{decode_log_bytes, decode_log_bytes_with_known_encoding};

/// 文件窗口结果；超长行返回前缀并明确标记，续读指向下一行，避免重复读取同一长行。
pub(super) struct FileWindow {
    pub text: String,
    pub encoding: String,
    pub line_count: usize,
    pub truncated: bool,
    pub line_truncated: bool,
    pub next_offset_line: Option<usize>,
}

/// 读取有界行窗口，任何 IO 或取消错误均返回模型可理解的英文原因。
pub(super) async fn read_file_window(
    path: &Path,
    preferred_encoding: &str,
    offset_line: usize,
    max_lines: usize,
    max_bytes: usize,
    cancellation: &CancellationToken,
) -> Result<FileWindow, String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|error| format!("cannot open file: {error}"))?;
    let mut sample = [0_u8; 4096];
    let sampled = file
        .read(&mut sample)
        .await
        .map_err(|error| format!("cannot sample file: {error}"))?;
    let encoding = decode_log_bytes(&sample[..sampled], preferred_encoding).encoding_label;
    let newline: &[u8] = match encoding.as_str() {
        "UTF-16LE" => &[b'\n', 0],
        "UTF-16BE" => &[0, b'\n'],
        _ => b"\n",
    };
    file.seek(std::io::SeekFrom::Start(0))
        .await
        .map_err(|error| format!("cannot seek file: {error}"))?;
    let mut reader = BufReader::new(file);
    let mut window = FileWindow {
        text: String::new(),
        encoding,
        line_count: 0,
        truncated: false,
        line_truncated: false,
        next_offset_line: None,
    };
    for _ in 1..offset_line {
        let skipped = read_bounded_line(&mut reader, 0, newline, cancellation).await?;
        if !skipped.has_content {
            return Ok(window);
        }
    }
    let mut bytes = Vec::with_capacity(max_bytes.min(8192));
    while window.line_count < max_lines && bytes.len() < max_bytes {
        let line =
            read_bounded_line(&mut reader, max_bytes - bytes.len(), newline, cancellation).await?;
        if !line.has_content {
            break;
        }
        window.line_count += 1;
        window.line_truncated |= line.truncated;
        bytes.extend_from_slice(&line.bytes);
        if line.truncated {
            break;
        }
    }
    let has_more = !reader
        .fill_buf()
        .await
        .map_err(|error| format!("cannot read file: {error}"))?
        .is_empty();
    window.truncated = has_more || window.line_truncated;
    window.next_offset_line = has_more.then(|| offset_line.saturating_add(window.line_count));
    window.text =
        decode_log_bytes_with_known_encoding(&bytes, &window.encoding, preferred_encoding).text;
    Ok(window)
}

struct BoundedLine {
    bytes: Vec<u8>,
    has_content: bool,
    truncated: bool,
}

/// 分块寻找完整换行符；保留量达到上限后只扫描/排空，不再增长 Vec。
async fn read_bounded_line(
    reader: &mut (impl AsyncBufRead + Unpin),
    limit: usize,
    newline: &[u8],
    cancellation: &CancellationToken,
) -> Result<BoundedLine, String> {
    let mut line = BoundedLine {
        bytes: Vec::with_capacity(limit.min(8192)),
        has_content: false,
        truncated: false,
    };
    let mut consumed = 0usize;
    let mut previous = None;
    loop {
        if cancellation.is_cancelled() {
            return Err("file read cancelled".to_string());
        }
        let chunk = reader
            .fill_buf()
            .await
            .map_err(|error| format!("cannot read file: {error}"))?;
        if chunk.is_empty() {
            return Ok(line);
        }
        let ending = if newline.len() == 1 {
            chunk
                .iter()
                .position(|byte| *byte == newline[0])
                .map(|index| index + 1)
        } else if consumed % 2 == 1 && previous == Some(newline[0]) && chunk[0] == newline[1] {
            Some(1)
        } else {
            // UTF-16 的换行必须位于完整代码单元边界，不能把字符中的 0x0a 当成换行。
            (consumed % 2..chunk.len().saturating_sub(1))
                .step_by(2)
                .find(|index| chunk[*index..*index + 2] == *newline)
                .map(|index| index + 2)
        };
        let count = ending.unwrap_or(chunk.len());
        let retained = count.min(limit.saturating_sub(line.bytes.len()));
        line.bytes.extend_from_slice(&chunk[..retained]);
        line.truncated |= retained < count;
        line.has_content = true;
        previous = Some(chunk[count - 1]);
        consumed = consumed.saturating_add(count);
        reader.consume(count);
        if ending.is_some() {
            return Ok(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 达到行数限制也必须返回续读偏移，不能把未读完的文件标为完整。
    #[tokio::test]
    async fn reports_line_limit_and_end_of_file() {
        let workspace = tempfile::tempdir().expect("应创建测试目录");
        let path = workspace.path().join("app.log");
        std::fs::write(&path, "one\ntwo\nthree\n").expect("应写入日志");
        let window = read_file_window(&path, "UTF-8", 1, 2, 1024, &CancellationToken::new())
            .await
            .expect("应读取窗口");
        assert_eq!(window.text, "one\ntwo\n");
        assert!(window.truncated);
        assert_eq!(window.next_offset_line, Some(3));
        let last = read_file_window(&path, "UTF-8", 3, 2, 1024, &CancellationToken::new())
            .await
            .expect("应读取末行");
        assert_eq!(last.text, "three\n");
        assert!(!last.truncated);
        assert_eq!(last.next_offset_line, None);
    }

    /// 超长行仅保留预算内前缀，续读不会卡在同一行；跳过时不保留任何正文。
    #[tokio::test]
    async fn long_lines_are_bounded_and_make_forward_progress() {
        let workspace = tempfile::tempdir().expect("应创建测试目录");
        let path = workspace.path().join("long.log");
        let content = format!("{}\ntail\n", "x".repeat(2 * 1024 * 1024));
        std::fs::write(&path, content).expect("应写入长行");
        let window = read_file_window(&path, "UTF-8", 1, 200, 64, &CancellationToken::new())
            .await
            .expect("应读取长行前缀");
        assert_eq!(window.text.len(), 64);
        assert!(window.line_truncated && window.truncated);
        assert_eq!(window.next_offset_line, Some(2));
        let tail = read_file_window(&path, "UTF-8", 2, 200, 64, &CancellationToken::new())
            .await
            .expect("应可跳过长行");
        assert_eq!(tail.text, "tail\n");
    }

    /// UTF-16 偏移读取复用文件编码，跨缓冲区的换行也保持代码单元对齐。
    #[tokio::test]
    async fn utf16_windows_keep_encoding_and_line_boundaries() {
        for little_endian in [true, false] {
            let workspace = tempfile::tempdir().expect("应创建测试目录");
            let path = workspace.path().join("utf16.log");
            let mut bytes = if little_endian {
                vec![255, 254]
            } else {
                vec![254, 255]
            };
            for unit in "one\ntwo\nthree\n".encode_utf16() {
                bytes.extend_from_slice(&if little_endian {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            std::fs::write(&path, &bytes).expect("应写入 UTF-16 日志");
            let window = read_file_window(&path, "UTF-8", 2, 1, 1024, &CancellationToken::new())
                .await
                .expect("应读取 UTF-16 窗口");
            assert_eq!(window.text, "two\n");
            assert_eq!(window.next_offset_line, Some(3));
            let newline = if little_endian { [10, 0] } else { [0, 10] };
            let mut reader = BufReader::with_capacity(3, &bytes[..]);
            let line = read_bounded_line(&mut reader, 1024, &newline, &CancellationToken::new())
                .await
                .expect("应识别跨块换行");
            assert_eq!(line.bytes.len(), 10);
        }
    }

    /// 文件缺失和取消均明确失败，不能返回默认成功。
    #[tokio::test]
    async fn read_errors_and_cancellation_are_reported() {
        let workspace = tempfile::tempdir().expect("应创建测试目录");
        let path = workspace.path().join("missing");
        assert!(
            read_file_window(&path, "UTF-8", 1, 1, 64, &CancellationToken::new())
                .await
                .is_err()
        );
        std::fs::write(&path, "content").expect("应写入测试文件");
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert!(
            read_file_window(&path, "UTF-8", 1, 1, 64, &cancellation)
                .await
                .is_err()
        );
    }
}
