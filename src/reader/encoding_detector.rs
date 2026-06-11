//! 文件职责：提供日志字节样本的编码检测与解码能力。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：识别 BOM、校验 UTF-8，并按用户配置编码兜底解码日志正文。

use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE};

/// 解码后的日志文本和实际采用的编码标签。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedText {
    /// 解码后的完整文本。
    pub text: String,
    /// 实际采用的编码名称，用于状态栏展示。
    pub encoding_label: String,
}

/// 将日志原始字节解码成文本。
///
/// 参数说明：
/// - `bytes`：日志原始字节，可来自 mmap 或压缩包流。
/// - `preferred_encoding`：用户设置中的默认编码名称，UTF-8 校验失败时作为兜底。
///
/// 返回值：解码文本与实际编码标签；无法精确识别时使用 UTF-8 有损兜底。
pub fn decode_log_bytes(bytes: &[u8], preferred_encoding: &str) -> DecodedText {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return decode_with_encoding(UTF_8, &bytes[3..], "UTF-8");
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return decode_with_encoding(UTF_16LE, &bytes[2..], "UTF-16LE");
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return decode_with_encoding(UTF_16BE, &bytes[2..], "UTF-16BE");
    }

    if let Ok(text) = std::str::from_utf8(bytes) {
        return DecodedText {
            text: text.to_string(),
            encoding_label: "UTF-8".to_string(),
        };
    }

    if let Some(encoding) = Encoding::for_label(preferred_encoding.trim().as_bytes()) {
        let label = encoding.name().to_string();
        return decode_with_encoding(encoding, bytes, &label);
    }

    let text = String::from_utf8_lossy(bytes).into_owned();
    DecodedText {
        text,
        encoding_label: "UTF-8-lossy".to_string(),
    }
}

/// 使用已知编码解码日志字节；仍优先处理 BOM，避免首行携带 BOM 时显示异常。
///
/// 参数说明：
/// - `bytes`：日志原始字节。
/// - `encoding_label`：前置检测得到的编码名称。
/// - `fallback_encoding`：编码名称无法识别时使用的兜底编码。
///
/// 返回值：解码文本与实际编码标签。
pub fn decode_log_bytes_with_known_encoding(
    bytes: &[u8],
    encoding_label: &str,
    fallback_encoding: &str,
) -> DecodedText {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF])
        || bytes.starts_with(&[0xFF, 0xFE])
        || bytes.starts_with(&[0xFE, 0xFF])
    {
        return decode_log_bytes(bytes, fallback_encoding);
    }

    if let Some(encoding) = Encoding::for_label(encoding_label.trim().as_bytes()) {
        let label = encoding.name().to_string();
        return decode_with_encoding(encoding, bytes, &label);
    }

    decode_log_bytes(bytes, fallback_encoding)
}

/// 使用指定 `encoding_rs` 编码执行解码，并统一包装返回结构。
fn decode_with_encoding(encoding: &'static Encoding, bytes: &[u8], label: &str) -> DecodedText {
    let (decoded, _, _) = encoding.decode(bytes);
    DecodedText {
        text: decoded.into_owned(),
        encoding_label: label.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::decode_log_bytes;

    /// 验证 UTF-8 BOM 会被识别并从正文中移除。
    #[test]
    fn decodes_utf8_bom_without_leading_marker() {
        let decoded = decode_log_bytes(&[0xEF, 0xBB, 0xBF, b'a', b'\n'], "UTF-8");

        assert_eq!(decoded.text, "a\n");
        assert_eq!(decoded.encoding_label, "UTF-8");
    }

    /// 验证非法 UTF-8 会按用户配置编码兜底，而不是直接失败。
    #[test]
    fn falls_back_to_preferred_encoding() {
        let decoded = decode_log_bytes(&[0xC4, 0xE3, 0xBA, 0xC3], "gbk");

        assert_eq!(decoded.text, "你好");
        assert_eq!(decoded.encoding_label, "GBK");
    }
}
