//! 文件职责：实现 Java 线程栈与 jstack 输出的专项高亮规则。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：识别线程名、线程状态、堆栈帧、锁对象、等待/阻塞和死锁提示。

use crate::highlight::rules::common::{
    find_ascii_case_insensitive_ranges, find_ascii_word_ranges, highlight_key_values,
    is_java_token_byte, push_dotted_token_segments, skip_ascii_spaces,
};
use crate::highlight::span::{HighlightTokenKind, SpanBuilder};

/// 高亮 Java 线程日志。
pub(crate) fn highlight_java_thread_dump(line: &str, builder: &mut SpanBuilder) {
    let trimmed_start = line.len() - line.trim_start().len();
    let trimmed = &line[trimmed_start..];
    let lower = line.to_ascii_lowercase();

    if lower.contains("deadlock") {
        builder.push(0, line.len(), HighlightTokenKind::Exception);
        return;
    }

    if let Some(thread_text) = trimmed.strip_prefix('"') {
        if let Some(end_quote) = thread_text.find('"') {
            builder.push(
                trimmed_start,
                trimmed_start + end_quote + 2,
                HighlightTokenKind::ThreadName,
            );
        }
        highlight_key_values(line, builder);
    }

    if let Some(state_start) = line.find("java.lang.Thread.State:") {
        push_dotted_token_segments(
            builder,
            state_start,
            "java.lang.Thread.State",
            HighlightTokenKind::Key,
        );
        let value_start = skip_ascii_spaces(
            line.as_bytes(),
            state_start + "java.lang.Thread.State:".len(),
        );
        let value_end = line[value_start..]
            .find(|character: char| character == '(' || character.is_whitespace())
            .map(|offset| value_start + offset)
            .unwrap_or(line.len());
        builder.push(value_start, value_end, HighlightTokenKind::ThreadState);
    }

    if trimmed.starts_with("at ") {
        highlight_stack_frame(line, trimmed_start + 3, builder);
    }

    for phrase in [
        "- locked",
        "- waiting on",
        "- waiting to lock",
        "- parking to wait for",
        "waiting on condition",
        "blocked",
    ] {
        for range in find_ascii_case_insensitive_ranges(line, phrase) {
            builder.push(range.start, range.end, HighlightTokenKind::Lock);
        }
    }
    highlight_angle_lock_ids(line, builder);
}

/// 高亮 Java 堆栈帧中的类、方法和文件行信息。
fn highlight_stack_frame(line: &str, method_path_start: usize, builder: &mut SpanBuilder) {
    let method_path_end = line[method_path_start..]
        .find('(')
        .map(|offset| method_path_start + offset)
        .unwrap_or(line.len());
    let method_path = &line[method_path_start..method_path_end];
    if let Some(method_dot) = method_path.rfind('.') {
        let class_end = method_path_start + method_dot;
        let method_start = class_end + 1;
        push_dotted_token_segments(
            builder,
            method_path_start,
            &line[method_path_start..class_end],
            HighlightTokenKind::StackClass,
        );
        builder.push(
            method_start,
            method_path_end,
            HighlightTokenKind::StackMethod,
        );
    } else {
        builder.push(
            method_path_start,
            method_path_end,
            HighlightTokenKind::StackMethod,
        );
    }

    if let Some(location_start) = line[method_path_end..].find('(') {
        let start = method_path_end + location_start;
        let end = line[start..]
            .find(')')
            .map(|offset| start + offset + 1)
            .unwrap_or(line.len());
        builder.push(start, end, HighlightTokenKind::StackLocation);
    }
}

/// 高亮 `<0x...>` 形式的锁对象。
fn highlight_angle_lock_ids(line: &str, builder: &mut SpanBuilder) {
    let bytes = line.as_bytes();
    let mut index = 0_usize;
    while let Some(relative) = bytes[index..].iter().position(|byte| *byte == b'<') {
        let start = index + relative;
        let Some(end_relative) = bytes[start..].iter().position(|byte| *byte == b'>') else {
            break;
        };
        let end = start + end_relative + 1;
        builder.push(start, end, HighlightTokenKind::Lock);
        index = end;
    }
}

/// 高亮异常相关关键字和异常类名，供普通日志规则复用。
pub(crate) fn highlight_exception_words(line: &str, builder: &mut SpanBuilder) {
    for range in find_ascii_case_insensitive_ranges(line, "Caused by") {
        builder.push(range.start, range.end, HighlightTokenKind::Exception);
    }

    for phrase in ["Exception", "Throwable"] {
        for range in find_ascii_word_ranges(line, phrase) {
            builder.push(range.start, range.end, HighlightTokenKind::Exception);
        }
    }

    let bytes = line.as_bytes();
    let mut index = 0_usize;
    while index < bytes.len() {
        if !is_java_token_byte(bytes[index]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && is_java_token_byte(bytes[index]) {
            index += 1;
        }
        let token = &line[start..index];
        if token.ends_with("Exception") || token.ends_with("Error") {
            push_dotted_token_segments(builder, start, token, HighlightTokenKind::Exception);
        }
    }
}
