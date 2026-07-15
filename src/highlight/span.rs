//! 文件职责：定义语法高亮的公共 token、span 和范围构造工具。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：为各类高亮规则提供统一的 UTF-8 字节范围模型与去重构造器。

use std::ops::Range;

/// 单行高亮最大扫描字节数，避免极端超长行拖慢滚动渲染。
pub(crate) const MAX_HIGHLIGHT_BYTES: usize = 16 * 1024;

/// 语法高亮 token 类型。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum HighlightTokenKind {
    /// TRACE 日志级别。
    Trace,
    /// DEBUG 日志级别。
    Debug,
    /// INFO 日志级别。
    Info,
    /// WARN/WARNING 日志级别。
    Warning,
    /// ERROR 日志级别。
    Error,
    /// FATAL 日志级别。
    Fatal,
    /// 日志时间戳。
    Timestamp,
    /// 注释文本。
    Comment,
    /// 配置键、字段名或标签关键部分。
    Key,
    /// 普通配置值。
    Value,
    /// 字符串值。
    String,
    /// 数字值。
    Number,
    /// 布尔值或 null。
    Boolean,
    /// 标点和结构符号。
    Punctuation,
    /// XML 标签名。
    Tag,
    /// XML 属性名。
    Attribute,
    /// Java 线程名。
    ThreadName,
    /// Java 线程状态。
    ThreadState,
    /// Java 堆栈类名。
    StackClass,
    /// Java 堆栈方法名。
    StackMethod,
    /// Java 堆栈文件行号位置。
    StackLocation,
    /// Java 锁对象或等待目标。
    Lock,
    /// 异常、错误和死锁提示。
    Exception,
}

/// 单个高亮范围，range 使用展示文本的 UTF-8 字节下标。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HighlightSpan {
    /// 高亮范围。
    pub range: Range<usize>,
    /// 高亮 token 类型。
    pub kind: HighlightTokenKind,
}

/// 高亮范围构造器，保证范围不重叠且不越界。
pub(crate) struct SpanBuilder {
    /// 当前行的有效扫描长度。
    line_len: usize,
    /// 已收集的高亮范围。
    spans: Vec<HighlightSpan>,
}

impl SpanBuilder {
    /// 创建构造器。
    ///
    /// 参数说明：
    /// - `line_len`：当前行参与高亮扫描的 UTF-8 字节长度。
    ///
    /// 返回值：空的高亮范围构造器。
    pub(crate) fn new(line_len: usize) -> Self {
        Self {
            line_len,
            spans: Vec::new(),
        }
    }

    /// 添加一个高亮范围；与已有高亮重叠时保留先加入的高优先级范围。
    ///
    /// 参数说明：
    /// - `start` / `end`：半开区间字节下标。
    /// - `kind`：范围对应的高亮 token 类型。
    pub(crate) fn push(&mut self, start: usize, end: usize, kind: HighlightTokenKind) {
        if start >= end || end > self.line_len {
            return;
        }
        let range = start..end;
        if self
            .spans
            .iter()
            .any(|span| ranges_overlap(&range, &span.range))
        {
            return;
        }
        self.spans.push(HighlightSpan { range, kind });
    }

    /// 输出按起点排序后的高亮范围。
    ///
    /// 返回值：不重叠且按起点升序排列的高亮范围。
    pub(crate) fn finish(mut self) -> Vec<HighlightSpan> {
        self.spans.sort_by_key(|span| span.range.start);
        self.spans
    }
}

/// 判断两个半开区间是否重叠。
pub(crate) fn ranges_overlap(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

/// 返回不切断 UTF-8 字符的扫描长度。
pub(crate) fn capped_scan_len(line: &str) -> usize {
    if line.len() <= MAX_HIGHLIGHT_BYTES {
        return line.len();
    }
    let mut end = MAX_HIGHLIGHT_BYTES;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    end
}
