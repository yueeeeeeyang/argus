//! 文件职责：实现 JavaScript 源代码单行高亮规则。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：识别 JavaScript 注释、字符串、模板文本、关键字、内置类型、函数和运算符。

use crate::highlight::rules::code_common::{CStyleLanguageSpec, highlight_c_style};
use crate::highlight::span::SpanBuilder;

/// JavaScript 关键字和常用上下文关键字。
const JAVASCRIPT_KEYWORDS: &[&str] = &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "finally",
    "for",
    "from",
    "function",
    "get",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "of",
    "return",
    "set",
    "static",
    "super",
    "switch",
    "throw",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
];
/// JavaScript 内置构造器与常见运行时类型。
const JAVASCRIPT_TYPES: &[&str] = &[
    "Array", "BigInt", "Boolean", "Date", "Error", "Function", "JSON", "Map", "Math", "Number",
    "Object", "Promise", "Proxy", "RegExp", "Set", "String", "Symbol", "WeakMap", "WeakSet",
];
/// JavaScript 特殊字面量。
const JAVASCRIPT_LITERALS: &[&str] = &["true", "false", "null", "undefined", "NaN", "Infinity"];

/// 高亮 JavaScript 代码行。
pub(crate) fn highlight_javascript(line: &str, builder: &mut SpanBuilder) {
    let spec = CStyleLanguageSpec {
        keywords: JAVASCRIPT_KEYWORDS,
        types: JAVASCRIPT_TYPES,
        literals: JAVASCRIPT_LITERALS,
        allow_backtick_string: true,
        allow_annotation: false,
        infer_capitalized_type: false,
    };
    highlight_c_style(line, builder, &spec);
}
