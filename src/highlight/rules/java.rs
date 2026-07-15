//! 文件职责：实现 Java 源代码单行高亮规则。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：识别 Java 注释、字符串、关键字、类型、注解、方法、数字和运算符。

use crate::highlight::rules::code_common::{CStyleLanguageSpec, highlight_c_style};
use crate::highlight::span::SpanBuilder;

/// Java 关键字；包含现代 Java 使用的模块、记录、密封类型和模式匹配相关词。
const JAVA_KEYWORDS: &[&str] = &[
    "abstract",
    "assert",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "else",
    "enum",
    "exports",
    "extends",
    "final",
    "finally",
    "for",
    "goto",
    "if",
    "implements",
    "import",
    "instanceof",
    "interface",
    "module",
    "native",
    "new",
    "non-sealed",
    "open",
    "opens",
    "package",
    "permits",
    "private",
    "protected",
    "provides",
    "public",
    "record",
    "requires",
    "return",
    "sealed",
    "static",
    "strictfp",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "to",
    "transient",
    "transitive",
    "try",
    "uses",
    "var",
    "volatile",
    "while",
    "with",
    "yield",
];
/// Java 原生类型与常见基础类型。
const JAVA_TYPES: &[&str] = &[
    "boolean",
    "byte",
    "char",
    "double",
    "float",
    "int",
    "long",
    "short",
    "void",
    "String",
    "Object",
    "Class",
    "Integer",
    "Long",
    "Double",
    "Float",
    "Boolean",
    "Character",
    "List",
    "Map",
    "Set",
    "Optional",
    "Stream",
];
/// Java 特殊字面量。
const JAVA_LITERALS: &[&str] = &["true", "false", "null"];

/// 高亮 Java 代码行。
pub(crate) fn highlight_java(line: &str, builder: &mut SpanBuilder) {
    let spec = CStyleLanguageSpec {
        keywords: JAVA_KEYWORDS,
        types: JAVA_TYPES,
        literals: JAVA_LITERALS,
        allow_backtick_string: false,
        allow_annotation: true,
        infer_capitalized_type: true,
    };
    highlight_c_style(line, builder, &spec);
}
