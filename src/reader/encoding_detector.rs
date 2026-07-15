//! 文件职责：提供日志字节样本的编码检测与解码能力。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：识别 BOM、UTF-8、编码声明与中文本地编码，并按用户配置编码兜底解码日志正文。

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::{BIG5, Encoding, GB18030, GBK, UTF_8, UTF_16BE, UTF_16LE};

/// 自动编码检测最多读取的样本大小；与分页日志采样策略保持一致，避免小日志整文件多轮扫描。
const LOG_ENCODING_SAMPLE_BYTES: usize = 4 * 1024 * 1024;

/// 解码后的日志文本和实际采用的编码标签。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DecodedText {
    /// 解码后的完整文本。
    pub text: String,
    /// 实际采用的编码名称，用于状态栏展示。
    pub encoding_label: String,
}

/// 将日志原始字节解码成文本。
///
/// 参数说明：
/// - `bytes`：日志原始字节，可来自 mmap 或压缩包流。
/// - `preferred_encoding`：自动识别完全失败时使用的用户配置兜底编码。
///
/// 返回值：解码文本与实际编码标签；无法精确识别时使用 UTF-8 有损兜底。
pub(crate) fn decode_log_bytes(bytes: &[u8], preferred_encoding: &str) -> DecodedText {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return decode_with_encoding(UTF_8, &bytes[3..], "UTF-8");
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return decode_with_encoding(UTF_16LE, &bytes[2..], "UTF-16LE");
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return decode_with_encoding(UTF_16BE, &bytes[2..], "UTF-16BE");
    }

    let sample_len = bytes.len().min(LOG_ENCODING_SAMPLE_BYTES);
    let detection_sample = &bytes[..sample_len];

    if let Some(encoding) = detect_log_encoding(detection_sample) {
        let label = encoding.name().to_string();
        let primary = decode_with_encoding_attempt(encoding, bytes, &label);

        if primary.has_decode_damage() {
            // 样本可能只有 ASCII 前缀，导致自动识别为 UTF-8；只有整文件解码真的出现
            // 替换字符时，才退回全量检测，避免每次打开小日志都多轮扫描整份文件。
            if let Some(rechecked_encoding) = detect_log_encoding(bytes) {
                let rechecked_label = rechecked_encoding.name().to_string();
                let rechecked =
                    decode_with_encoding_attempt(rechecked_encoding, bytes, &rechecked_label);
                if rechecked.replacement_count < primary.replacement_count {
                    return rechecked.into_decoded_text();
                }
            }
        }

        return primary.into_decoded_text();
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
pub(crate) fn decode_log_bytes_with_known_encoding(
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
        let primary = decode_with_encoding_attempt(encoding, bytes, &label);
        if primary.has_decode_damage() {
            // 大日志分页读取时，首个采样块可能只有 ASCII，导致整份文件的已知编码被误判为
            // UTF-8。逐行解码一旦出现替换字符，就重新执行自动识别，避免后续中文长期乱码。
            let repaired = decode_log_bytes(bytes, fallback_encoding);
            if count_replacement_characters(&repaired.text) < primary.replacement_count {
                return repaired;
            }
        }
        return primary.into_decoded_text();
    }

    decode_log_bytes(bytes, fallback_encoding)
}

/// 使用指定 `encoding_rs` 编码执行解码，并统一包装返回结构。
fn decode_with_encoding(encoding: &'static Encoding, bytes: &[u8], label: &str) -> DecodedText {
    decode_with_encoding_attempt(encoding, bytes, label).into_decoded_text()
}

/// 单次解码尝试及其质量信号，用于在分页逐行读取阶段自修复误判编码。
#[derive(Debug)]
struct DecodeAttempt {
    /// 解码后的文本。
    text: String,
    /// 本次尝试使用的编码标签。
    label: String,
    /// `encoding_rs` 是否报告解码错误。
    had_errors: bool,
    /// U+FFFD 替换字符数量，直接反映用户看到的乱码程度。
    replacement_count: usize,
}

impl DecodeAttempt {
    /// 判断本次解码是否出现可见损伤。
    fn has_decode_damage(&self) -> bool {
        self.had_errors || self.replacement_count > 0
    }

    /// 转换为公开返回结构。
    fn into_decoded_text(self) -> DecodedText {
        DecodedText {
            text: self.text,
            encoding_label: self.label,
        }
    }
}

/// 使用指定编码执行一次解码，并统计替换字符数量。
fn decode_with_encoding_attempt(
    encoding: &'static Encoding,
    bytes: &[u8],
    label: &str,
) -> DecodeAttempt {
    let (decoded, had_errors) = encoding.decode_without_bom_handling(bytes);
    let text = decoded.into_owned();
    let replacement_count = count_replacement_characters(&text);

    DecodeAttempt {
        text,
        label: label.to_string(),
        had_errors,
        replacement_count,
    }
}

/// 统计替换字符数量，作为编码误判后是否需要自修复的直接依据。
fn count_replacement_characters(text: &str) -> usize {
    text.chars()
        .filter(|character| *character == '\u{FFFD}')
        .count()
}

/// 参考 logclinic3 的自动识别策略，在受控编码集合内选择最可靠的日志编码。
///
/// 识别顺序：
/// - 严格 UTF-8 成功时直接返回 UTF-8。
/// - 使用 chardetng 做统计检测，但只接受 UTF-8、GBK、GB18030、Big5。
/// - GBK 与 GB18030 重叠时，发现 GB18030 四字节序列会提升为 GB18030。
/// - 统计结果不可用时，再尝试中文日志常见编码兜底。
fn detect_log_encoding(bytes: &[u8]) -> Option<&'static Encoding> {
    if decode_without_replacement(bytes, UTF_8) {
        return Some(UTF_8);
    }

    let mut detector = EncodingDetector::new(Iso2022JpDetection::Deny);
    detector.feed(bytes, true);
    let guessed = detector.guess(None, Utf8Detection::Allow);

    if let Some(candidate) = encoding_from_detector_guess(guessed) {
        if candidate == GBK
            && contains_gb18030_four_byte_sequence(bytes)
            && decode_without_replacement(bytes, GB18030)
        {
            return Some(GB18030);
        }

        if candidate == GBK
            && !decode_without_replacement(bytes, GBK)
            && decode_without_replacement(bytes, GB18030)
        {
            return Some(GB18030);
        }

        if decode_without_replacement(bytes, candidate) {
            return Some(candidate);
        }
    }

    [GB18030, GBK, BIG5]
        .into_iter()
        .find(|candidate| decode_without_replacement(bytes, candidate))
}

/// 判断指定编码能否无替换字符地解码样本。
fn decode_without_replacement(bytes: &[u8], encoding: &'static Encoding) -> bool {
    let (_, had_errors) = encoding.decode_without_bom_handling(bytes);
    !had_errors
}

/// 将 chardetng 的猜测结果收敛到 Argus 明确支持的日志编码集合。
fn encoding_from_detector_guess(encoding: &'static Encoding) -> Option<&'static Encoding> {
    if encoding == UTF_8 {
        Some(UTF_8)
    } else if encoding == GBK {
        Some(GBK)
    } else if encoding == GB18030 {
        Some(GB18030)
    } else if encoding == BIG5 {
        Some(BIG5)
    } else {
        None
    }
}

/// 判断字节流是否包含 GB18030 独有的四字节编码形态。
///
/// GB18030 兼容 GBK 的双字节区间，仅靠“能否解码”会把部分 GB18030 误标为 GBK；
/// 四字节序列 `81-FE 30-39 81-FE 30-39` 是强信号，发现后提升为 GB18030。
fn contains_gb18030_four_byte_sequence(bytes: &[u8]) -> bool {
    bytes.windows(4).any(|window| {
        matches!(window[0], 0x81..=0xFE)
            && matches!(window[1], 0x30..=0x39)
            && matches!(window[2], 0x81..=0xFE)
            && matches!(window[3], 0x30..=0x39)
    })
}

#[cfg(test)]
mod tests {
    use super::{decode_log_bytes, decode_log_bytes_with_known_encoding};
    use encoding_rs::Encoding;

    /// 验证 UTF-8 BOM 会被识别并从正文中移除。
    #[test]
    fn decodes_utf8_bom_without_leading_marker() {
        let decoded = decode_log_bytes(&[0xEF, 0xBB, 0xBF, b'a', b'\n'], "UTF-8");

        assert_eq!(decoded.text, "a\n");
        assert_eq!(decoded.encoding_label, "UTF-8");
    }

    /// 验证非法 UTF-8 会进入中文本地编码自动识别，而不是直接失败。
    #[test]
    fn detects_chinese_local_encoding_for_invalid_utf8() {
        let decoded = decode_log_bytes(&[0xC4, 0xE3, 0xBA, 0xC3], "gbk");

        assert_eq!(decoded.text, "你好");
        assert_eq!(decoded.encoding_label, "gb18030");
    }

    /// 验证默认编码仍为 UTF-8 时，非法 UTF-8 的 GBK 中文日志会自动识别为 GBK。
    #[test]
    fn detects_gbk_when_default_encoding_is_utf8() {
        let gbk = Encoding::for_label(b"gbk").expect("GBK 编码应存在");
        let (bytes, _, _) = gbk.encode("#缓存数量\nrecordcount=120000\n");
        let decoded = decode_log_bytes(&bytes, "UTF-8");

        assert_eq!(decoded.text, "#缓存数量\nrecordcount=120000\n");
        assert_eq!(decoded.encoding_label, "gb18030");
    }

    /// 验证 GBK 日志中出现 `encode=GBK` 普通文本时仍按内容自动识别，而不是解析声明。
    #[test]
    fn detects_gbk_even_when_text_mentions_encode_label() {
        let gbk = Encoding::for_label(b"gbk").expect("GBK 编码应存在");
        let (bytes, _, _) = gbk.encode("encode=GBK\n#缓存策略\n");
        let decoded = decode_log_bytes(&bytes, "UTF-8");

        assert_eq!(decoded.text, "encode=GBK\n#缓存策略\n");
        assert_eq!(decoded.encoding_label, "gb18030");
    }

    /// 验证 GBK 日志正文中出现误导性的 `charset=UTF-8` 字样时不会被强制按 UTF-8 解码。
    #[test]
    fn detects_gbk_when_text_mentions_charset_utf8() {
        let gbk = Encoding::for_label(b"gbk").expect("GBK 编码应存在");
        let expected = "request charset=UTF-8\n==OA->MES工单定时查询生产001==";
        let (bytes, _, _) = gbk.encode(expected);
        let decoded = decode_log_bytes(&bytes, "UTF-8");

        assert_eq!(decoded.text, expected);
        assert_eq!(decoded.encoding_label, "GBK");
    }

    /// 验证合法 UTF-8 不会被中文编码候选误判。
    #[test]
    fn keeps_valid_utf8_as_utf8() {
        let decoded = decode_log_bytes("#缓存数量\nrecordcount=120000\n".as_bytes(), "UTF-8");

        assert_eq!(decoded.text, "#缓存数量\nrecordcount=120000\n");
        assert_eq!(decoded.encoding_label, "UTF-8");
    }

    /// 验证合法 UTF-8 日志中出现 `encode=GBK` 字样时，不会被误当成编码声明。
    #[test]
    fn keeps_valid_utf8_even_when_text_mentions_gbk() {
        let decoded = decode_log_bytes("encode=GBK\nrecordcount=120000\n".as_bytes(), "UTF-8");

        assert_eq!(decoded.text, "encode=GBK\nrecordcount=120000\n");
        assert_eq!(decoded.encoding_label, "UTF-8");
    }

    /// 验证 GB18030 四字节扩展字符不会被误标为 GBK。
    #[test]
    fn detects_gb18030_four_byte_extension() {
        let gb18030 = Encoding::for_label(b"gb18030").expect("GB18030 编码应存在");
        let (bytes, _, _) = gb18030.encode("INFO 𠀀\n");
        let decoded = decode_log_bytes(&bytes, "UTF-8");

        assert_eq!(decoded.text, "INFO 𠀀\n");
        assert_eq!(decoded.encoding_label, "gb18030");
    }

    /// 验证 Big5 繁体中文样本能通过统计检测识别。
    #[test]
    fn detects_big5_when_default_encoding_is_utf8() {
        let big5 = Encoding::for_label(b"big5").expect("Big5 编码应存在");
        let (bytes, _, _) = big5.encode("WARN 繁體日誌\n");
        let decoded = decode_log_bytes(&bytes, "UTF-8");

        assert_eq!(decoded.text, "WARN 繁體日誌\n");
        assert_eq!(decoded.encoding_label, "Big5");
    }

    /// 验证分页逐行读取时，已知编码误判为 UTF-8 后能根据替换字符自修复为 GBK。
    #[test]
    fn known_utf8_line_repairs_to_gbk_when_replacements_appear() {
        let gbk = Encoding::for_label(b"gbk").expect("GBK 编码应存在");
        let expected = "定时任务同步党组织人员信息开始";
        let (bytes, _, _) = gbk.encode(expected);
        let decoded = decode_log_bytes_with_known_encoding(&bytes, "UTF-8", "UTF-8");

        assert_eq!(decoded.text, expected);
        assert_eq!(decoded.encoding_label, "GBK");
    }

    /// 验证分页逐行读取时，错误的繁体编码标签也能根据替换字符自修复为 GBK。
    #[test]
    fn known_big5_line_repairs_to_gbk_when_replacements_appear() {
        let gbk = Encoding::for_label(b"gbk").expect("GBK 编码应存在");
        let expected = "ERROR 定时任务同步党组织人员信息结束";
        let (bytes, _, _) = gbk.encode(expected);
        let decoded = decode_log_bytes_with_known_encoding(&bytes, "Big5", "UTF-8");

        assert_eq!(decoded.text, expected);
        assert_eq!(decoded.encoding_label, "GBK");
    }
}
