//! 文件职责：实现 SQL 脚本单行高亮规则。
//! 创建日期：2026-07-15
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：分类 SQL 保留字、类型、参数、数字和运算符；注释与字符串边界由统一词法层处理。

use crate::highlight::rules::code_common::{
    contains_word, is_code_identifier_start, scan_code_identifier, scan_code_number, scan_operator,
};
use crate::highlight::rules::common::skip_ascii_spaces;
use crate::highlight::span::{HighlightTokenKind, SpanBuilder};

/// 跨常见数据库方言的 SQL 核心关键字。
const SQL_KEYWORDS: &[&str] = &[
    "add",
    "all",
    "alter",
    "and",
    "as",
    "asc",
    "begin",
    "between",
    "by",
    "case",
    "commit",
    "constraint",
    "create",
    "cross",
    "database",
    "default",
    "delete",
    "desc",
    "distinct",
    "drop",
    "else",
    "end",
    "exists",
    "from",
    "full",
    "grant",
    "group",
    "having",
    "in",
    "index",
    "inner",
    "insert",
    "intersect",
    "into",
    "is",
    "join",
    "left",
    "like",
    "limit",
    "not",
    "nulls",
    "offset",
    "on",
    "or",
    "order",
    "outer",
    "over",
    "partition",
    "primary",
    "references",
    "returning",
    "revoke",
    "right",
    "rollback",
    "row",
    "rows",
    "select",
    "set",
    "table",
    "then",
    "truncate",
    "union",
    "unique",
    "update",
    "using",
    "values",
    "view",
    "when",
    "where",
    "with",
];
/// SQL 常见字段类型。
const SQL_TYPES: &[&str] = &[
    "bigint",
    "binary",
    "bit",
    "blob",
    "boolean",
    "char",
    "clob",
    "date",
    "datetime",
    "decimal",
    "double",
    "float",
    "int",
    "integer",
    "json",
    "nchar",
    "numeric",
    "nvarchar",
    "real",
    "smallint",
    "text",
    "time",
    "timestamp",
    "tinyint",
    "uuid",
    "varbinary",
    "varchar",
];
/// SQL 布尔与空值字面量。
const SQL_LITERALS: &[&str] = &["true", "false", "null", "unknown"];

/// 分类统一词法层屏蔽注释与字符串后的 SQL 代码。
pub(crate) fn highlight_sql(line: &str, builder: &mut SpanBuilder) {
    let bytes = line.as_bytes();
    let mut index = 0_usize;
    while index < bytes.len() {
        // 字符串与注释由统一词法扫描器处理，代码分类不再重复判断边界。
        match bytes[index] {
            b':' | b'@'
                if bytes
                    .get(index + 1)
                    .is_some_and(|byte| is_code_identifier_start(*byte)) =>
            {
                let end = scan_code_identifier(bytes, index + 1);
                builder.push(index, end, HighlightTokenKind::Variable);
                index = end;
            }
            b'0'..=b'9' => {
                let end = scan_code_number(bytes, index);
                builder.push(index, end, HighlightTokenKind::Number);
                index = end;
            }
            byte if is_code_identifier_start(byte) => {
                let end = scan_code_identifier(bytes, index);
                let token = &line[index..end];
                let next = skip_ascii_spaces(bytes, end);
                let kind = if contains_word(SQL_LITERALS, token, true) {
                    Some(HighlightTokenKind::Boolean)
                } else if contains_word(SQL_KEYWORDS, token, true) {
                    Some(HighlightTokenKind::Keyword)
                } else if contains_word(SQL_TYPES, token, true) {
                    Some(HighlightTokenKind::Type)
                } else if bytes.get(next) == Some(&b'(') {
                    Some(HighlightTokenKind::Function)
                } else {
                    None
                };
                if let Some(kind) = kind {
                    builder.push(index, end, kind);
                }
                index = end;
            }
            b'(' | b')' | b',' | b';' | b'.' => {
                builder.push(index, index + 1, HighlightTokenKind::Punctuation);
                index += 1;
            }
            b'=' | b'+' | b'-' | b'*' | b'/' | b'%' | b'!' | b'<' | b'>' | b'|' | b'&' => {
                let end = scan_operator(bytes, index);
                builder.push(index, end, HighlightTokenKind::Operator);
                index = end;
            }
            _ => index += 1,
        }
    }
}
