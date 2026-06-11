//! 文件职责：实现普通日志行的高亮规则。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：识别时间戳、日志等级、异常、key-value 字段和 Java 类名片段。

use crate::highlight::rules::common::{
    find_ascii_word_ranges, highlight_key_values, is_java_identifier_start, is_java_token_byte,
    push_dotted_token_segments,
};
use crate::highlight::rules::java_thread::{highlight_exception_words, highlight_java_thread_dump};
use crate::highlight::span::{HighlightTokenKind, SpanBuilder};

/// 高亮普通日志；Java 线程栈规则作为增强项默认启用。
pub(crate) fn highlight_log(line: &str, builder: &mut SpanBuilder) {
    highlight_java_thread_dump(line, builder);
    highlight_log_timestamps(line, builder);
    highlight_log_levels(line, builder);
    highlight_exception_words(line, builder);
    highlight_key_values(line, builder);
    highlight_java_class_like_tokens(line, builder);
}

/// 高亮日志时间戳。
fn highlight_log_timestamps(line: &str, builder: &mut SpanBuilder) {
    let bytes = line.as_bytes();
    for start in 0..bytes.len().saturating_sub(9) {
        if is_date_at(bytes, start) {
            let mut end = start + 10;
            if bytes
                .get(end)
                .is_some_and(|byte| matches!(*byte, b' ' | b'T'))
            {
                let time_start = end + 1;
                if is_time_at(bytes, time_start) {
                    end = time_start + 8;
                    if bytes.get(end) == Some(&b'.') {
                        end += 1;
                        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                            end += 1;
                        }
                    }
                    if bytes
                        .get(end)
                        .is_some_and(|byte| matches!(*byte, b'Z' | b'z'))
                    {
                        end += 1;
                    }
                }
            }
            builder.push(start, end, HighlightTokenKind::Timestamp);
        }
    }
}

/// 判断指定位置是否为 yyyy-MM-dd 或 yyyy/MM/dd。
fn is_date_at(bytes: &[u8], start: usize) -> bool {
    bytes.get(start..start + 10).is_some_and(|date| {
        date[0..4].iter().all(u8::is_ascii_digit)
            && matches!(date[4], b'-' | b'/')
            && date[5..7].iter().all(u8::is_ascii_digit)
            && matches!(date[7], b'-' | b'/')
            && date[8..10].iter().all(u8::is_ascii_digit)
    })
}

/// 判断指定位置是否为 HH:mm:ss。
fn is_time_at(bytes: &[u8], start: usize) -> bool {
    bytes.get(start..start + 8).is_some_and(|time| {
        time[0..2].iter().all(u8::is_ascii_digit)
            && time[2] == b':'
            && time[3..5].iter().all(u8::is_ascii_digit)
            && time[5] == b':'
            && time[6..8].iter().all(u8::is_ascii_digit)
    })
}

/// 高亮常见日志等级。
fn highlight_log_levels(line: &str, builder: &mut SpanBuilder) {
    for (level, kind) in [
        ("TRACE", HighlightTokenKind::Trace),
        ("DEBUG", HighlightTokenKind::Debug),
        ("INFO", HighlightTokenKind::Info),
        ("WARN", HighlightTokenKind::Warning),
        ("WARNING", HighlightTokenKind::Warning),
        ("ERROR", HighlightTokenKind::Error),
        ("FATAL", HighlightTokenKind::Fatal),
    ] {
        for range in find_ascii_word_ranges(line, level) {
            builder.push(range.start, range.end, kind);
        }
    }
}

/// 高亮 Java 类名形态的 token。
fn highlight_java_class_like_tokens(line: &str, builder: &mut SpanBuilder) {
    let bytes = line.as_bytes();
    let mut index = 0_usize;
    while index < bytes.len() {
        if !is_java_identifier_start(bytes[index]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && is_java_token_byte(bytes[index]) {
            index += 1;
        }
        let token = &line[start..index];
        if token.contains('.') && token.chars().any(char::is_uppercase) {
            push_dotted_token_segments(builder, start, token, HighlightTokenKind::StackClass);
        }
    }
}
