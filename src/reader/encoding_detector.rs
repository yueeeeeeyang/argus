//! 文件职责：提供日志字节样本的编码检测与解码能力。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：识别 BOM、UTF-8、编码声明与中文本地编码，并按用户配置编码兜底解码日志正文。

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

    if let Some(encoding) = detect_declared_encoding(bytes) {
        let label = encoding.name().to_string();
        return decode_with_encoding(encoding, bytes, &label);
    }

    if let Some(decoded) = decode_with_best_fallback(bytes, preferred_encoding) {
        return decoded;
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

/// 从 ASCII 兼容的日志样本中识别 `encoding`、`charset` 或 `encode` 声明。
fn detect_declared_encoding(bytes: &[u8]) -> Option<&'static Encoding> {
    let sample_len = bytes.len().min(64 * 1024);
    let mut normalized = Vec::with_capacity(sample_len);
    for byte in &bytes[..sample_len] {
        normalized.push(byte.to_ascii_lowercase());
    }

    for key in [
        b"charset".as_slice(),
        b"encoding".as_slice(),
        b"encode".as_slice(),
    ] {
        let mut search_from = 0_usize;
        while let Some(relative) = find_ascii(&normalized[search_from..], key) {
            let key_start = search_from + relative;
            if let Some(label_start) = skip_encoding_separator(&normalized, key_start + key.len()) {
                let label_end = normalized[label_start..]
                    .iter()
                    .position(|byte| !is_encoding_label_byte(*byte))
                    .map(|offset| label_start + offset)
                    .unwrap_or(normalized.len());
                if label_end > label_start {
                    if let Some(encoding) = Encoding::for_label(&normalized[label_start..label_end])
                    {
                        return Some(encoding);
                    }
                }
            }
            search_from = key_start.saturating_add(key.len());
        }
    }

    None
}

/// 查找 ASCII 子串；样本很小，直接窗口扫描更直观，也避免引入正则依赖。
fn find_ascii(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// 跳过编码声明键和值之间的空白、等号、冒号和引号。
fn skip_encoding_separator(bytes: &[u8], mut index: usize) -> Option<usize> {
    while let Some(byte) = bytes.get(index) {
        match *byte {
            b' ' | b'\t' | b'=' | b':' | b'"' | b'\'' => index += 1,
            _ => break,
        }
    }
    (index < bytes.len()).then_some(index)
}

/// 判断字节是否可以作为编码标签的一部分。
fn is_encoding_label_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

/// 在严格 UTF-8 失败后，通过多候选解码评分选择最不像乱码的编码。
fn decode_with_best_fallback(bytes: &[u8], preferred_encoding: &str) -> Option<DecodedText> {
    let mut candidates = Vec::new();
    push_encoding_candidate(&mut candidates, preferred_encoding);
    push_encoding_candidate(&mut candidates, "GBK");
    push_encoding_candidate(&mut candidates, "GB18030");
    push_encoding_candidate(&mut candidates, "Big5");
    push_encoding_candidate(&mut candidates, "windows-1252");

    candidates
        .into_iter()
        .enumerate()
        .map(|(order, encoding)| score_decoded_candidate(order, encoding, bytes))
        .min_by_key(|candidate| candidate.sort_key())
        .map(|candidate| DecodedText {
            text: candidate.text,
            encoding_label: candidate.label,
        })
}

/// 添加候选编码并按编码规范名称去重。
fn push_encoding_candidate(candidates: &mut Vec<&'static Encoding>, label: &str) {
    let Some(encoding) = Encoding::for_label(label.trim().as_bytes()) else {
        return;
    };
    if candidates
        .iter()
        .any(|existing| existing.name().eq_ignore_ascii_case(encoding.name()))
    {
        return;
    }
    candidates.push(encoding);
}

/// 解码候选及其评分结果；分数越低越可信。
#[derive(Debug)]
struct DecodedCandidate {
    /// 候选顺序，用于评分完全相同时保持优先级稳定。
    order: usize,
    /// 解码后的文本。
    text: String,
    /// 编码标签。
    label: String,
    /// 是否发生解码错误。
    had_errors: bool,
    /// U+FFFD 替换字符数量，越多越可能是乱码。
    replacement_count: usize,
    /// 非换行制表的控制字符数量。
    control_count: usize,
    /// 常见 CJK 字符数量，用于识别中文日志。
    cjk_count: usize,
}

impl DecodedCandidate {
    /// 返回排序分数；优先减少替换字符和控制字符，再偏向包含中文的候选。
    fn sort_key(&self) -> (usize, usize, usize, usize, usize) {
        let error_penalty = usize::from(self.had_errors);
        let cjk_bonus = self.cjk_count.min(1024);
        (
            self.replacement_count.saturating_mul(10_000),
            error_penalty,
            self.control_count.saturating_mul(128),
            usize::MAX.saturating_sub(cjk_bonus),
            self.order,
        )
    }
}

/// 对指定编码执行解码并统计乱码特征。
fn score_decoded_candidate(
    order: usize,
    encoding: &'static Encoding,
    bytes: &[u8],
) -> DecodedCandidate {
    let (decoded, _, had_errors) = encoding.decode(bytes);
    let text = decoded.into_owned();
    let mut replacement_count = 0_usize;
    let mut control_count = 0_usize;
    let mut cjk_count = 0_usize;

    for character in text.chars() {
        if character == '\u{FFFD}' {
            replacement_count += 1;
        } else if character.is_control() && !matches!(character, '\n' | '\r' | '\t') {
            control_count += 1;
        } else if is_cjk_character(character) {
            cjk_count += 1;
        }
    }

    DecodedCandidate {
        order,
        text,
        label: encoding.name().to_string(),
        had_errors,
        replacement_count,
        control_count,
        cjk_count,
    }
}

/// 判断字符是否属于常见中文、日文、韩文统一表意文字范围。
fn is_cjk_character(character: char) -> bool {
    matches!(
        character as u32,
        0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0x20000..=0x2A6DF
            | 0x2A700..=0x2B73F
            | 0x2B740..=0x2B81F
            | 0x2B820..=0x2CEAF
    )
}

#[cfg(test)]
mod tests {
    use super::decode_log_bytes;
    use encoding_rs::Encoding;

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

    /// 验证默认编码仍为 UTF-8 时，非法 UTF-8 的 GBK 中文日志会自动识别为 GBK。
    #[test]
    fn detects_gbk_when_default_encoding_is_utf8() {
        let gbk = Encoding::for_label(b"gbk").expect("GBK 编码应存在");
        let (bytes, _, _) = gbk.encode("#缓存数量\nrecordcount=120000\n");
        let decoded = decode_log_bytes(&bytes, "UTF-8");

        assert_eq!(decoded.text, "#缓存数量\nrecordcount=120000\n");
        assert_eq!(decoded.encoding_label, "GBK");
    }

    /// 验证日志内显式声明 `encode=GBK` 时会优先使用声明编码。
    #[test]
    fn detects_declared_gbk_encoding() {
        let gbk = Encoding::for_label(b"gbk").expect("GBK 编码应存在");
        let (bytes, _, _) = gbk.encode("encode=GBK\n#缓存策略\n");
        let decoded = decode_log_bytes(&bytes, "UTF-8");

        assert_eq!(decoded.text, "encode=GBK\n#缓存策略\n");
        assert_eq!(decoded.encoding_label, "GBK");
    }

    /// 验证合法 UTF-8 不会被中文编码候选误判。
    #[test]
    fn keeps_valid_utf8_as_utf8() {
        let decoded = decode_log_bytes("#缓存数量\nrecordcount=120000\n".as_bytes(), "UTF-8");

        assert_eq!(decoded.text, "#缓存数量\nrecordcount=120000\n");
        assert_eq!(decoded.encoding_label, "UTF-8");
    }

    /// 验证业务文本中出现 `encode=GBK` 时，合法 UTF-8 文件仍按 UTF-8 显示。
    #[test]
    fn keeps_valid_utf8_even_when_text_mentions_gbk() {
        let decoded = decode_log_bytes("encode=GBK\n#缓存策略\n".as_bytes(), "UTF-8");

        assert_eq!(decoded.text, "encode=GBK\n#缓存策略\n");
        assert_eq!(decoded.encoding_label, "UTF-8");
    }
}
