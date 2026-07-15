//! 文件职责：组织各语法类型的高亮规则模块。
//! 创建日期：2026-06-11
//! 修改日期：2026-07-15
//! 作者：Argus 开发团队
//! 主要功能：按格式拆分日志、配置、Java 线程栈和常见代码的高亮实现，保持模块边界清晰。

pub(crate) mod code_common;
pub(crate) mod common;
pub(crate) mod css;
pub(crate) mod java;
pub(crate) mod java_thread;
pub(crate) mod javascript;
pub(crate) mod json;
pub(crate) mod jsp;
pub(crate) mod log;
pub(crate) mod properties;
pub(crate) mod shell;
pub(crate) mod sql;
pub(crate) mod xml;
pub(crate) mod yaml;
